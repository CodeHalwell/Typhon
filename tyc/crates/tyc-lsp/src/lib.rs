//! Language Server Protocol backend for Typhon.
//!
//! Implements a `tower-lsp-server` backend that re-runs the type-check
//! pipeline whenever a file is opened or changed and publishes the resulting
//! diagnostics back to the editor. Hover, go-to-definition, completion, and
//! code-action requests are served from a cached [`ResolvedModule`] so
//! repeated queries on unchanged files skip the name-resolution pass.

use std::collections::HashMap;
use std::sync::Arc;

use miette::{Diagnostic as MietteDiagnostic, LabeledSpan};
use salsa::Setter;
use tokio::sync::Mutex;
use tower_lsp_server::ls_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, CodeActionResponse, CompletionItem, CompletionItemKind,
    CompletionOptions, CompletionParams, CompletionResponse, Diagnostic, DiagnosticSeverity,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    Documentation, GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents, HoverParams,
    HoverProviderCapability, InitializeParams, InitializeResult, InitializedParams, Location,
    MarkupContent, MarkupKind, MessageType, NumberOrString, OneOf, Position, Range,
    SemanticTokensFullOptions, SemanticTokensOptions, SemanticTokensParams, SemanticTokensResult,
    SemanticTokensServerCapabilities, ServerCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextEdit, Uri, WorkspaceEdit,
};
use tower_lsp_server::{jsonrpc, Client, LanguageServer, LspService, Server};
use tyc_db::{
    check_source_file, check_source_file_with_imports, module_shapes_query, preprocessed_full,
    preprocessed_text, resolved_module_arc, ModuleShapes, SourceFile, TycDatabase,
};
use tyc_diagnostics::TycError;
use tyc_resolve::{
    BindingKind, ClassKind, ImportInfo, Mutability, ResolveOptions, ResolvedModule, SymbolAtOffset,
};

mod semantic;
mod stdlib_stubs;
mod venv_introspect;

/// Manual resolved-module cache for cross-file import resolution.
///
/// Keyed by file:// URI; value is `(preprocessed_text, Arc<ResolvedModule>)`.
/// Used only for target files that may not be open in the editor (no Salsa
/// `SourceFile` handle).  Same-file hover/definition/completion use the
/// Salsa-tracked `resolved_module` query instead.
type ResolvedCache = Arc<Mutex<HashMap<String, (String, Arc<ResolvedModule>)>>>;

/// The Typhon LSP backend. Holds a single shared salsa database and the
/// `Client` handle used to send notifications back to the editor.
///
/// The database is wrapped in [`Arc<tokio::sync::Mutex<_>>`] so the
/// `check_source_file` call can run on a blocking executor thread without
/// pinning the async runtime — concurrent `hover` and `shutdown`
/// requests stay responsive while a file is being checked.
pub struct Backend {
    client: Client,
    db: Arc<Mutex<TycDatabase>>,
    log_level: LogLevel,
    /// Per-document Salsa handles, keyed by URI string.
    ///
    /// The [`SourceFile`] handle is the Salsa input for this document; on
    /// `did_change` it is updated via `set_text` so Salsa propagates
    /// invalidations incrementally rather than creating a fresh entity.
    /// Raw text is stored inside the Salsa input itself and read back via
    /// `source_file.text(db)` when needed, avoiding dual-state synchronisation.
    documents: Arc<Mutex<HashMap<String, SourceFile>>>,
    /// Resolved-module cache for cross-file import resolution.
    ///
    /// Stores resolved modules for target files that may not be open in the
    /// editor (no Salsa SourceFile).  Keyed by file:// URI; evicted when
    /// the preprocessed text changes.
    resolved_cache: ResolvedCache,
    /// Per-project-root introspection caches. Keyed on the directory
    /// containing the project's `typhon.toml`; value is a Mutex-guarded
    /// cache that shells to `.venv/bin/python` on misses and remembers
    /// the result. One cache per project keeps unrelated workspaces
    /// from sharing module results — different venvs have different
    /// installed packages.
    ///
    /// The *inner* mutex is a `std::sync::Mutex` (not a `tokio::Mutex`)
    /// because every consumer holds it across a synchronous subprocess
    /// call, and `tokio::Mutex::blocking_lock` panics from an async
    /// context. The whole introspection block is wrapped in
    /// `tokio::task::spawn_blocking` at the call site so the
    /// single-threaded LSP runtime keeps serving unrelated requests.
    introspection: Arc<
        Mutex<
            HashMap<std::path::PathBuf, Arc<std::sync::Mutex<venv_introspect::IntrospectionCache>>>,
        >,
    >,
    /// Per-project-root signature caches for *type checking* (distinct from
    /// `introspection`, which serves member completion). Holds a
    /// [`tyc_venv::VenvSignatures`] so live diagnostics flag wrong-typed /
    /// wrong-arity third-party calls — the same enrichment `tyc check` /
    /// `tyc build` perform, reused across keystrokes (the cache invalidates
    /// itself on `.venv` change). Same tokio-outer / std-inner locking as
    /// `introspection`: the enrichment shells to Python inside
    /// `spawn_blocking`.
    signature_caches:
        Arc<Mutex<HashMap<std::path::PathBuf, Arc<std::sync::Mutex<tyc_venv::VenvSignatures>>>>>,
    /// Per-project-root auto-import index. Keyed on the directory
    /// containing the project's `typhon.toml`; value is a Mutex-guarded
    /// `ProjectIndex` that's refreshed lazily on every completion
    /// request. Same locking shape as `introspection`: tokio outer,
    /// std inner because refresh + parse runs inside `spawn_blocking`.
    project_indexes: Arc<Mutex<HashMap<std::path::PathBuf, Arc<std::sync::Mutex<ProjectIndex>>>>>,
    /// Per-project-root index of project source files registered with
    /// the Salsa database. Used by the cross-module shape lookup so
    /// `module_shapes_query` can cache extraction on file-text basis:
    /// a keystroke in `src/main.ty` doesn't re-parse `src/clients.ty`.
    ///
    /// Outer key: project root path (the directory containing
    /// `typhon.toml`). Inner: dotted module name (e.g. `"foo.bar"`)
    /// → Salsa `SourceFile` handle.
    ///
    /// Entries are refreshed lazily inside `check_and_publish`: every
    /// `.ty` / `.dty` file under the project's src tree is registered
    /// (or its text re-uploaded via `set_text`) before the cross-
    /// module shape map is assembled.
    project_files: Arc<Mutex<HashMap<std::path::PathBuf, HashMap<String, SourceFile>>>>,
    /// Last document version we've already kicked off a venv-
    /// introspection prewarm for, keyed by URI. Used to debounce
    /// the per-keystroke prewarm: when the editor sends ten
    /// `did_change` events in a row, only the first spawns the
    /// background introspection task. Subsequent calls bail out as
    /// soon as the version matches.
    ///
    /// Keyed by full URI string so two files in the same project
    /// don't share state. `None` version is treated as "always run"
    /// because `tower-lsp-server` reserves the unversioned shape
    /// for the synthetic-open form (`source_file_for` injecting an
    /// untracked file).
    prewarmed_versions: Arc<Mutex<HashMap<String, i32>>>,
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend").finish_non_exhaustive()
    }
}

impl Backend {
    /// True if a message at `level` should be forwarded to the editor.
    fn should_log(&self, level: MessageType) -> bool {
        // `MessageType` ranking, low-to-high: LOG < INFO < WARNING < ERROR.
        // We treat LOG as the debug-only channel.
        let rank = |m: MessageType| match m {
            MessageType::ERROR => 4,
            MessageType::WARNING => 3,
            MessageType::INFO => 2,
            MessageType::LOG => 1,
            _ => 2,
        };
        let threshold = match self.log_level {
            LogLevel::Error => 4,
            LogLevel::Warn => 3,
            LogLevel::Info => 2,
            LogLevel::Debug => 1,
        };
        rank(level) >= threshold
    }

    async fn log(&self, level: MessageType, msg: impl Into<String>) {
        if self.should_log(level) {
            self.client.log_message(level, msg.into()).await;
        }
    }

    /// Run the check pipeline on `text` and publish any resulting diagnostics
    /// (warnings + errors) back to the editor. `version` is forwarded so the
    /// editor can drop stale results.
    ///
    /// The check itself runs inside [`tokio::task::spawn_blocking`] because
    /// the compiler pipeline is CPU-bound and synchronous; keeping it off
    /// the runtime thread lets other LSP requests (hover, shutdown) make
    /// progress concurrently.
    async fn check_and_publish(&self, uri: Uri, text: String, version: Option<i32>) {
        let uri_str = uri.as_str().to_owned();
        let db = Arc::clone(&self.db);

        // Upsert the SourceFile in the Salsa database.
        //
        // On `did_open` we create a fresh entity; on `did_change` we call
        // `set_text` on the existing handle so Salsa can propagate the
        // invalidation incrementally: queries that depend on this file's text
        // are re-evaluated on the next access, but queries for *other* files
        // stay cached.  We hold the db lock only for this short operation so
        // hover/completion requests can still acquire it while the heavy
        // `check_source_file` runs on the blocking thread.
        let source_file: SourceFile = {
            let existing = {
                let docs = self.documents.lock().await;
                docs.get(&uri_str).copied()
            };
            let mut db_guard = self.db.lock().await;
            if let Some(sf) = existing {
                sf.set_text(&mut *db_guard).to(text.clone());
                sf
            } else {
                SourceFile::new(&*db_guard, uri_str.clone(), text.clone())
            }
        };

        // Cache the Salsa handle; raw text is stored inside Salsa itself and
        // retrieved via `source_file.text(db)` when needed.
        {
            let mut docs = self.documents.lock().await;
            docs.insert(uri_str.clone(), source_file);
        }

        // Resolve the project root once on the async side so the
        // blocking closure (which holds the salsa db lock) only needs
        // the cheap filesystem walk + cached queries below.
        let path_for_root = std::path::PathBuf::from(uri.path().as_str());
        let workspace = find_workspace_layout(&path_for_root);
        let project_files_arc = Arc::clone(&self.project_files);

        // Per-project venv signature caches, for folding third-party shapes
        // into the check so wrong-typed / wrong-arity third-party calls show
        // up as live diagnostics. Only the Arc is cloned here on the async
        // side; the get-or-create (which reads `typhon.toml` and scans the
        // venv) and the Python-shelling enrichment all run inside the blocking
        // closure below, off the async executor.
        let signature_caches_arc = Arc::clone(&self.signature_caches);

        let text_for_check = text.clone();
        let uri_str_for_check = uri_str.clone();
        let result = tokio::task::spawn_blocking(move || {
            // Hold the mutex only for the duration of the salsa call.
            let mut db = db.blocking_lock();
            // Build the project-wide shape registry inside the
            // blocking closure so the salsa-cached
            // `module_shapes_query` does the heavy lifting: only the
            // file whose text actually changed re-runs the parse.
            // `set_text` on a salsa input is a no-op when the new
            // value matches, so re-uploading every file's on-disk
            // text per check doesn't churn the cache. The currently-
            // edited document uses its in-flight buffer text, not
            // the on-disk content, so cross-module diagnostics
            // update within one keystroke.
            let project_shapes = if let Some((_root, src_dir)) = workspace.as_ref() {
                let src_root_name = src_dir
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("src")
                    .to_owned();
                #[allow(clippy::explicit_auto_deref)]
                let mut shapes = build_project_shapes_salsa(
                    &mut *db,
                    &project_files_arc,
                    src_dir,
                    &src_root_name,
                    &uri_str_for_check,
                    &text_for_check,
                );
                // Fold venv-introspected third-party shapes into the project
                // map so the cross-module check flags wrong-typed / -arity
                // calls to installed dependencies live in the editor. All of
                // this — reading `typhon.toml`, scanning the venv, and the
                // Python-shelling introspection — runs on this blocking thread.
                // The persistent per-project cache means a keystroke only
                // shells to Python when a new dependency module appears or the
                // venv changed. (`enrich_into`'s import scan reads `.ty` files
                // from disk — a freshly-typed, unsaved `import` is picked up on
                // the next save.)
                if let Some(root) = workspace.as_ref().map(|(r, _)| r) {
                    let allowed = tyc_venv::allowed_top_level_from_project(root);
                    if !allowed.is_empty() {
                        let sig_cache = {
                            let mut caches = signature_caches_arc.blocking_lock();
                            let entry = caches.entry(root.clone()).or_insert_with(|| {
                                Arc::new(std::sync::Mutex::new(
                                    tyc_venv::VenvSignatures::for_project_root(
                                        root,
                                        allowed.clone(),
                                    ),
                                ))
                            });
                            // Keep the allow-list current if deps changed.
                            if let Ok(mut vs) = entry.lock() {
                                vs.set_allowed_top_level(allowed);
                            }
                            Arc::clone(entry)
                        };
                        if let Ok(mut vs) = sig_cache.lock() {
                            let project_module_set: std::collections::HashSet<String> =
                                shapes.keys().cloned().collect();
                            let _ = vs.enrich_into(
                                std::slice::from_ref(src_dir),
                                &project_module_set,
                                &mut shapes,
                            );
                        };
                    }
                }
                std::sync::Arc::new(shapes)
            } else {
                std::sync::Arc::new(std::collections::HashMap::new())
            };
            #[allow(clippy::explicit_auto_deref)]
            let diags = if project_shapes.is_empty() {
                // No workspace layout discovered — fall back to the
                // Salsa-cached per-file check so isolated edits keep
                // the LSP cache warm.
                check_source_file(&mut *db, source_file)
            } else {
                // Cross-module-aware check, backed by the Salsa
                // `module_shapes_query` cache: unchanged sibling
                // files reuse the cached extraction, so a keystroke
                // in the open file only re-parses that one file.
                // Use the SourceFile-backed entry point so the
                // tracked `preprocessed_full` / `resolved_module`
                // queries can return cached values for unchanged
                // files. The LSP already holds `source_file` across
                // edits, which is the input that gates the cache.
                // (`uri_str_for_check` / `text_for_check` are
                // consumed in the `build_project_shapes_salsa` call
                // above so we don't need to thread them again here.)
                check_source_file_with_imports(&mut *db, source_file, &project_shapes)
            };
            // Retrieve the preprocessed source for diagnostic position
            // mapping.  After `check_source_file` runs the full pipeline the
            // Salsa `preprocessed_text` query is populated; hover/definition
            // handlers that call it afterward benefit from the cache.
            let mapping_source = preprocessed_text(&*db, source_file);
            (diags, mapping_source)
        })
        .await;

        let (diags, mapping_source) = match result {
            Ok(value) => value,
            Err(e) => {
                self.log(
                    MessageType::ERROR,
                    format!("tyc-lsp: check task panicked: {e}"),
                )
                .await;
                return;
            }
        };

        let mut out = Vec::with_capacity(diags.error_count() + diags.warning_count());
        for err in diags.errors() {
            let src = diagnostic_source(err, &text, &mapping_source);
            if let Some(d) = tyc_error_to_lsp(err, src, DiagnosticSeverity::ERROR) {
                out.push(d);
            }
        }
        for warn in diags.warnings() {
            let src = diagnostic_source(warn, &text, &mapping_source);
            if let Some(d) = tyc_error_to_lsp(warn, src, DiagnosticSeverity::WARNING) {
                out.push(d);
            }
        }

        let uri_for_prewarm = uri.clone();
        self.client.publish_diagnostics(uri, out, version).await;

        // Pre-warm the venv-introspection cache for every third-party
        // import in the open document. Runs as a detached background
        // task so it never blocks diagnostics publishing — by the
        // time the user hovers a third-party symbol, the
        // `cache.members(...)` call inside `hover_import_extras` hits
        // a populated entry and returns in microseconds instead of
        // shelling to Python.
        //
        // Debounced per-URI by the document version: the LSP fires
        // a `did_change` on every keystroke, but we only need to
        // re-walk the imports once per *content* change, and even
        // then the cache lookups are idempotent. Without this gate
        // a fast typist would queue dozens of `spawn_blocking` tasks
        // before the first one had a chance to populate the cache.
        self.spawn_introspection_prewarm(uri_for_prewarm, version)
            .await;
    }

    /// Spawn a detached task that introspects every third-party
    /// import in the document at `uri`, populating the cache so
    /// later hover requests don't have to wait on a subprocess.
    ///
    /// Debounced via `prewarmed_versions`: when the editor sends
    /// many `did_change` events in rapid succession, only the first
    /// for each unique version spawns a new task. Subsequent calls
    /// at the same version short-circuit at the version check.
    async fn spawn_introspection_prewarm(&self, uri: Uri, version: Option<i32>) {
        // Version-based debounce. `None` means an untracked open
        // (rare; the synthetic-document path), in which case we run
        // unconditionally — there's no version to dedupe against.
        if let Some(v) = version {
            let uri_str = uri.as_str().to_owned();
            let mut versions = self.prewarmed_versions.lock().await;
            if versions.get(&uri_str) == Some(&v) {
                return;
            }
            versions.insert(uri_str, v);
        }
        let Some(sf) = self.source_file_for(&uri).await else {
            return;
        };
        let resolved = {
            let db = self.db.lock().await;
            resolved_module_arc(&*db, sf)
        };
        // Collect dotted module names referenced through import
        // bindings in the module's top-level scope. Skip relative
        // imports (resolver writes them as `.` / `..foo`) — those are
        // project-local and don't go through venv introspection.
        //
        // For each import, prewarm BOTH the bare module path and any
        // dotted submodule the user typed. Without the submodule
        // pass, `import torch.nn as nn` would only prewarm `torch`
        // (a noisy `dir(torch)` call) and the user's first `nn.<dot>`
        // would block on the slow first-import of `torch.nn`. Pre-
        // warming `torch.nn` directly here means the completion path
        // hits a cached entry.
        let mut modules: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some(scope0) = resolved.scopes.first() {
            for binding in &scope0.bindings {
                if let Some(info) = &binding.import_info {
                    if info.module.starts_with('.') || info.module.is_empty() {
                        continue;
                    }
                    // Always warm the full dotted module path the user
                    // referenced (`torch.nn`, `numpy.linalg`).
                    modules.insert(info.module.clone());
                    // `from foo.bar import baz` may also expose `baz`
                    // as a submodule — warm `foo.bar.baz` in case it
                    // resolves to a module rather than a class/value.
                    // The introspection cache records the success/
                    // failure either way, so a non-module member here
                    // costs one subprocess and is forgotten.
                    if let Some(member) = info.member.as_deref() {
                        if !member.is_empty()
                            && member
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || c == '_')
                        {
                            modules.insert(format!("{}.{}", info.module, member));
                        }
                    }
                }
            }
        }
        if modules.is_empty() {
            return;
        }
        let Some((root, cache)) = self.introspection_cache_for(&uri).await else {
            return;
        };
        tokio::task::spawn_blocking(move || {
            // Sort for determinism — same warm order on every open
            // makes log output easier to read when debugging cold
            // hovers, and stable cache-insert order pins cross-test
            // expectations.
            let mut modules: Vec<String> = modules.into_iter().collect();
            modules.sort();
            for module in modules {
                // Recover from a poisoned cache rather than aborting
                // the rest of the warmup. A panic during a previous
                // introspection shouldn't permanently disable
                // hover-extras for every subsequent module.
                let mut guard = match cache.lock() {
                    Ok(g) => g,
                    Err(p) => p.into_inner(),
                };
                // `members()` is idempotent — already-cached entries
                // (success *or* failure) short-circuit; only the
                // first call per module per session pays the
                // subprocess cost.
                let _ = guard.members(&root, &module);
            }
        });
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> jsonrpc::Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions {
                    // Trigger on `.` so member access pulls up class/instance
                    // bindings (currently we just return the visible-name list;
                    // the LSP client filters by prefix as the user types).
                    trigger_characters: Some(vec![".".to_string()]),
                    ..Default::default()
                }),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                // Advertise semantic tokens with the legend declared in
                // `semantic::legend()`. Indices into that legend are
                // baked into the token stream, so changing the order
                // would force every theme to re-bind colours — keep
                // it stable across releases.
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: semantic::legend(),
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            ..Default::default()
                        },
                    ),
                ),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.log(MessageType::INFO, "tyc-lsp ready").await;
    }

    async fn shutdown(&self) -> jsonrpc::Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let doc = params.text_document;
        self.check_and_publish(doc.uri, doc.text, Some(doc.version))
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        // Full-text sync: the spec guarantees a single `content_changes`
        // entry whose `text` is the new buffer when we advertise FULL sync.
        let Some(change) = params.content_changes.into_iter().next() else {
            return;
        };
        self.check_and_publish(
            params.text_document.uri,
            change.text,
            Some(params.text_document.version),
        )
        .await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        // Clear diagnostics when the user closes a file so stale errors do
        // not linger in the editor.
        let uri = params.text_document.uri;
        let uri_str = uri.as_str().to_owned();
        {
            let mut docs = self.documents.lock().await;
            docs.remove(&uri_str);
        }
        {
            // Drop the per-document debounce entry so reopening the
            // file re-runs the prewarm (the venv may have changed
            // between open / close / reopen via `uv sync`).
            let mut versions = self.prewarmed_versions.lock().await;
            versions.remove(&uri_str);
        }
        self.evict_resolved_cache(&uri_str).await;
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }

    async fn hover(&self, params: HoverParams) -> jsonrpc::Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let Some(sf) = self.source_file_for(&uri).await else {
            return Ok(None);
        };
        // Both `preprocessed_text` and `resolved_module` are Salsa-tracked
        // queries that hit the cache when the file hasn't changed since the
        // last `check_source_file` call.
        let (preprocessed, resolved) = {
            let db = self.db.lock().await;
            (preprocessed_text(&*db, sf), resolved_module_arc(&*db, sf))
        };
        let offset = position_to_byte(&preprocessed, position);
        let Some(symbol) = resolved.symbol_at_offset(offset) else {
            return Ok(None);
        };

        // Build the base hover body (kind + name + declaration-site
        // marker) from the resolver's view of the symbol. When the
        // symbol points at a third-party import, enrich it with the
        // package's real signature + docstring recovered through the
        // venv introspection cache — this is the "what is this
        // thing?" preview the user expects when hovering a foreign
        // class or function. Project / stdlib symbols fall through to
        // the base body unchanged.
        //
        // Rendered as `MarkupContent { kind: Markdown, … }` so the
        // editor formats fenced code blocks, italics, and headings
        // properly. `MarkedString::String` (the older shape) is
        // treated as plain text by most clients including VS Code.
        let mut body = render_hover(&symbol);
        if let Some(import_extras) = self.hover_import_extras(&uri, &symbol).await {
            body.push_str("\n\n");
            body.push_str(&import_extras);
        }
        let range = Some(Range {
            start: byte_to_position(&preprocessed, symbol.span.0),
            end: byte_to_position(&preprocessed, symbol.span.1),
        });
        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: body,
            }),
            range,
        }))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> jsonrpc::Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let Some(sf) = self.source_file_for(&uri).await else {
            return Ok(None);
        };
        // Pull the preprocessed source + resolved bindings + full
        // preprocess result + raw on-disk text out of Salsa — all
        // tracked queries, so the second call on an unchanged file is
        // a cache hit. Cloning the `Arc` is cheap. The full result
        // carries the per-line strip metadata the semantic-tokens
        // remap pass needs to translate preprocessed-source columns
        // back to original-source columns so colours land on the right
        // characters when `pub` / `comptime` / `freeze` / `lazy`
        // modifiers shifted them. `salsa_text` is the SourceFile's
        // stored text — used as a fallback when the editor buffer
        // entry has gone away (e.g. concurrent close); falling back to
        // the *preprocessed* text would defeat the remap, since the
        // whole point is to translate into the original Typhon source.
        let (preprocessed, resolved, prep_full, salsa_text) = {
            let db = self.db.lock().await;
            (
                preprocessed_text(&*db, sf),
                resolved_module_arc(&*db, sf),
                preprocessed_full(&*db, sf),
                sf.text(&*db).clone(),
            )
        };
        // Original (pre-preprocess) source: read from the editor's open
        // buffer when we have it, fall back to the SourceFile's stored
        // Typhon text. The semantic-tokens client uses this to render
        // colours, so we want the freshest version of the buffer.
        let original = self.document_text(&uri).await.unwrap_or(salsa_text);
        let line_shifts = prep_full.0.line_col_shifts();
        // Parse the preprocessed source for the AST walk
        // (attribute-access tokens). `parse_module` is fast — the
        // type checker already does it on every check pass — but the
        // result isn't currently Salsa-cached for the LSP, so we
        // re-parse here. Cheap enough at the file sizes we expect;
        // can be promoted to a tracked query later if profiling
        // says it matters.
        let module = match tyc_syntax::parse_module(&preprocessed) {
            Ok(p) => p.into_syntax(),
            Err(_) => {
                // Parse errors are surfaced through diagnostics
                // already; the semantic-tokens stream stays empty so
                // the editor falls back to the TextMate grammar
                // without a confusing partial colouring.
                return Ok(Some(SemanticTokensResult::Tokens(
                    tower_lsp_server::ls_types::SemanticTokens::default(),
                )));
            }
        };
        let stdlib = tyc_resolve::python_stdlib_modules();
        // Build the callee → signature map the kwarg pass consults.
        // Walks the resolver's top-level bindings to find every
        // imported class / function name, then asks the introspection
        // cache for its signature so `Agent(client=…)` can colour
        // `client` orange when it really is on the Agent constructor.
        // Cache miss / no venv / parse failure → empty entry, which
        // disables kwarg colouring for that callee (white) without
        // affecting the rest of the file.
        let (callee_signatures, attribute_kinds) =
            self.build_callee_signatures(&uri, &resolved).await;
        let tokens = semantic::compute_with_original(
            &preprocessed,
            &original,
            &line_shifts,
            &resolved,
            &module,
            stdlib,
            &callee_signatures,
            &attribute_kinds,
        );
        Ok(Some(SemanticTokensResult::Tokens(tokens)))
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> jsonrpc::Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let Some(sf) = self.source_file_for(&uri).await else {
            return Ok(None);
        };
        let (preprocessed, resolved) = {
            let db = self.db.lock().await;
            (preprocessed_text(&*db, sf), resolved_module_arc(&*db, sf))
        };
        // Mid-type buffers (`os.<cursor>`) typically don't parse — the
        // trailing dot is a syntax error. The cached `ResolvedModule`
        // returned above is empty in that state, so imports aren't
        // visible to `compute_completion_items` and aliased lookups
        // (`import numpy as np; np.<cursor>`) fail. Patch the source
        // with a placeholder identifier after the cursor's bare `.`
        // and re-resolve so the resolver can see the rest of the
        // module. We only pay this cost when the cached resolution is
        // empty (parse failure), so happy-path edits don't take the
        // hit.
        let (preprocessed, resolved) = if resolved.scopes.is_empty() {
            try_fixup_and_resolve(&preprocessed, position)
                .map(|(p, r)| (p, Arc::new(r)))
                .unwrap_or((preprocessed, resolved))
        } else {
            (preprocessed, resolved)
        };
        // Pre-fetch introspection results for the candidate modules
        // *before* dropping into the synchronous completion path. The
        // cache shells to `.venv/bin/python`, which is a blocking
        // operation — calling it from inside the async handler would
        // wedge the LSP's single-threaded runtime, and the previous
        // `tokio::Mutex::blocking_lock` shape panicked outright.
        // `spawn_blocking` parks the work on Tokio's blocking thread
        // pool while the runtime keeps serving other requests.
        let cursor_offset = position_to_byte(&preprocessed, position);
        let receiver = extract_member_access_receiver(&preprocessed, cursor_offset);
        let from_import = extract_from_import_module(&preprocessed, cursor_offset);
        // `candidate_module_paths` already returns unique paths, and the
        // other two branches yield at most one element each — no dedup
        // pass is needed.
        let candidates: Vec<String> = match (&receiver, &from_import) {
            (Some(r), _) => candidate_module_paths(&resolved, r, cursor_offset),
            (None, Some(m)) => vec![m.clone()],
            (None, None) => Vec::new(),
        };
        // Resolve project src_dir + the venv introspection cache up
        // front (both need async locks), then hand the resulting handles
        // to a single `spawn_blocking` task that does *all* the
        // blocking work — file reads + parse + resolve for project
        // modules, and the `.venv/bin/python` shell for venv ones.
        // Doing both in one task keeps the LSP runtime responsive and
        // avoids running synchronous compiler work on the async
        // executor.
        let project_src_dir = uri_to_path(&uri)
            .and_then(|p| find_workspace_layout(&p))
            .map(|(_, src)| src);
        let venv_cache = self.introspection_cache_for(&uri).await;
        let prefetched: HashMap<String, Vec<CompletionItem>> = if candidates.is_empty() {
            HashMap::new()
        } else {
            let candidates_for_task = candidates.clone();
            tokio::task::spawn_blocking(move || {
                let mut map: HashMap<String, Vec<CompletionItem>> = HashMap::new();
                // Pass 1: project files. First-party `.ty`s win over
                // venv-installed packages with the same import name
                // (PYTHONPATH semantics) — we resolve them first and
                // skip the venv pass for anything already populated.
                //
                // When the file *exists* but parse/resolve fails (a
                // common mid-keystroke state on the file you're
                // editing), claim the module for the project anyway
                // by inserting an empty list. Falling through to venv
                // would surface a *different* third-party package
                // with the same import name — better to show "no
                // suggestions" until the file parses again than to
                // mislead with unrelated symbols.
                if let Some(src_dir) = project_src_dir.as_ref() {
                    for module in &candidates_for_task {
                        if resolve_module_to_file(src_dir, module).is_some() {
                            let items = project_module_members(src_dir, module).unwrap_or_default();
                            map.insert(module.clone(), items);
                        }
                    }
                }
                // Pass 2: venv introspection for whatever the project
                // didn't cover.
                if let Some((root, cache)) = venv_cache {
                    // `lock()` on a poisoned std::Mutex returns `Err`;
                    // recover the data anyway — poisoning came from a
                    // panic in an earlier completion, which we recorded
                    // as a `None` cache entry, so the next request just
                    // re-runs the lookup.
                    let mut guard = match cache.lock() {
                        Ok(g) => g,
                        Err(p) => p.into_inner(),
                    };
                    for module in &candidates_for_task {
                        if map.contains_key(module) {
                            continue;
                        }
                        if let Some(members) = guard.members(&root, module) {
                            map.insert(
                                module.clone(),
                                introspected_members_to_completion(&members),
                            );
                        }
                    }
                }
                map
            })
            .await
            .unwrap_or_default()
        };
        let introspect_closure =
            |module: &str| -> Option<Vec<CompletionItem>> { prefetched.get(module).cloned() };
        let introspect_ref: Option<&IntrospectFn<'_>> = if prefetched.is_empty() {
            None
        } else {
            Some(&introspect_closure)
        };
        let mut items = compute_completion_items_with_introspection(
            &resolved,
            &preprocessed,
            position,
            introspect_ref,
        );

        // Auto-import suggestions: only meaningful in open-completion
        // context (the receiver / from-import branches already return
        // their own focused menus). For each top-level public symbol
        // declared in some sibling `.ty` we don't currently have in
        // scope, append a `CompletionItem` whose `additionalTextEdits`
        // inserts the corresponding import when the user accepts.
        if receiver.is_none() && from_import.is_none() {
            if let Some((src_dir, index)) = self.project_index_for(&uri).await {
                let raw_source = self.document_text(&uri).await.unwrap_or_default();
                let current_module =
                    uri_to_path(&uri).and_then(|p| compute_module_path(&src_dir, &p));
                let in_scope: std::collections::HashSet<String> =
                    items.iter().map(|i| i.label.clone()).collect();
                // Scan the file *once* for the import-insertion anchor;
                // every suggested symbol reuses the same range, just
                // with a different `new_text`. Without this each
                // suggestion would re-scan the entire file, making
                // open-completion in large projects O(symbols × lines).
                let insertion_range = auto_import_insertion_range(&raw_source);
                let auto_import_items = tokio::task::spawn_blocking(move || {
                    let mut guard = match index.lock() {
                        Ok(g) => g,
                        Err(p) => p.into_inner(),
                    };
                    guard.refresh(&src_dir);
                    let mut out: Vec<CompletionItem> = Vec::new();
                    for (name, entries) in &guard.by_name {
                        if in_scope.contains(name) {
                            continue;
                        }
                        for entry in entries {
                            // Don't suggest importing from the file we
                            // are editing — the user can just write the
                            // name directly.
                            if Some(&entry.module) == current_module.as_ref() {
                                continue;
                            }
                            out.push(CompletionItem {
                                label: name.clone(),
                                kind: Some(binding_kind_to_completion_kind(entry.kind)),
                                detail: Some(format!("from {}", entry.module)),
                                additional_text_edits: Some(vec![auto_import_text_edit_at(
                                    insertion_range,
                                    &entry.module,
                                    name,
                                )]),
                                // Sort auto-import items below the
                                // in-scope / keyword / builtin menu so
                                // the editor prefers names the user has
                                // already imported.
                                sort_text: Some(format!("z{name}")),
                                ..Default::default()
                            });
                        }
                    }
                    out
                })
                .await
                .unwrap_or_default();
                items.extend(auto_import_items);
            }
        }

        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn code_action(
        &self,
        params: CodeActionParams,
    ) -> jsonrpc::Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        // code_action only needs raw text (no preprocessing); read it from
        // the documents cache without touching the db.
        let Some(text) = self.document_text(&uri).await else {
            return Ok(None);
        };
        let actions = compute_code_actions(&uri, &text, &params.context.diagnostics);
        if actions.is_empty() {
            Ok(None)
        } else {
            Ok(Some(actions))
        }
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let Some(sf) = self.source_file_for(&uri).await else {
            return Ok(None);
        };
        let (preprocessed, resolved) = {
            let db = self.db.lock().await;
            (preprocessed_text(&*db, sf), resolved_module_arc(&*db, sf))
        };
        let offset = position_to_byte(&preprocessed, position);
        let Some(symbol) = resolved.symbol_at_offset(offset) else {
            return Ok(None);
        };
        let Some(def) = symbol.definition else {
            return Ok(None);
        };

        // Cross-file go-to-definition: when the local declaration site is
        // an `import` binding, resolve the module path back to a sibling
        // `.ty` source in the workspace and jump to the member's
        // declaration there.  Falls through to the local declaration span
        // when the import can't be resolved on disk (e.g. third-party
        // stdlib, missing source file).
        if def.kind == BindingKind::Import {
            if let Some(info) = &def.import_info {
                if let Some(loc) = self.resolve_cross_file_import(&uri, info).await {
                    return Ok(Some(GotoDefinitionResponse::Scalar(loc)));
                }
            }
        }

        let location = Location {
            uri,
            range: Range {
                start: byte_to_position(&preprocessed, def.span.0),
                end: byte_to_position(&preprocessed, def.span.1),
            },
        };
        Ok(Some(GotoDefinitionResponse::Scalar(location)))
    }
}

impl Backend {
    /// Collect every top-level imported class / function name from
    /// `resolved`, look each one up in the per-project introspection
    /// cache, and parse the signature into a [`semantic::CalleeSignature`].
    /// Returns an empty map when there's no venv / cache for this
    /// URI — kwarg colouring degrades to "no token", which is the
    /// honest default for "we don't know what this callable accepts".
    ///
    /// Runs off the async runtime so the synchronous `Mutex::lock` +
    /// potential cold subprocess spawn inside `members()` can't stall
    /// the LSP. The prewarm pass at document open / change keeps the
    /// cache hot, so this is usually a fast hash lookup.
    async fn build_callee_signatures(
        &self,
        uri: &Uri,
        resolved: &tyc_resolve::ResolvedModule,
    ) -> (semantic::CalleeSignatures, semantic::AttributeKinds) {
        use semantic::{AttributeKinds, CalleeSignatures};
        let mut out = CalleeSignatures::new();
        let mut attr_kinds = AttributeKinds::new();
        let Some((root, cache)) = self.introspection_cache_for(uri).await else {
            return (out, attr_kinds);
        };
        // Two shapes of import the kwarg classifier handles:
        //
        // 1. `from X import Y` — the binding name `Y` is the callee
        //    name we want to colour kwargs for. Lookup key in the
        //    final map is just `Y`.
        // 2. `import X` (and `import X as Z`) — the binding alone
        //    isn't callable; the call site is `X.Foo(…)` /
        //    `Z.Foo(…)`. We fetch the *entire* member list for
        //    module X and emit one map entry per public member,
        //    keyed `Z.Foo` so the attribute-callee path in
        //    `AstWalker::callee_signature_for` can construct the
        //    same key from the AST.
        //
        // Bare-import expansion makes the map a bit bigger (a
        // module with 50 public symbols → 50 entries), but the
        // venv cache is already warm by the time semantic-tokens
        // requests come in, so this is just memcpy work.
        let mut named_wanted: Vec<(String, String, String)> = Vec::new();
        let mut bare_wanted: Vec<(String, String)> = Vec::new();
        if let Some(module_scope) = resolved.scopes.first() {
            for binding in &module_scope.bindings {
                if !matches!(binding.kind, tyc_resolve::BindingKind::Import) {
                    continue;
                }
                let Some(info) = &binding.import_info else {
                    continue;
                };
                match &info.member {
                    Some(member) => {
                        named_wanted.push((
                            binding.name.clone(),
                            info.module.clone(),
                            member.clone(),
                        ));
                    }
                    None => {
                        bare_wanted.push((binding.name.clone(), info.module.clone()));
                    }
                }
            }
        }
        if named_wanted.is_empty() && bare_wanted.is_empty() {
            return (out, attr_kinds);
        }
        let mut by_module: std::collections::HashMap<String, Vec<(String, String)>> =
            std::collections::HashMap::new();
        for (binding_name, module, member) in named_wanted {
            by_module
                .entry(module)
                .or_default()
                .push((binding_name, member));
        }
        let bare_by_module: std::collections::HashMap<String, Vec<String>> = bare_wanted
            .into_iter()
            .fold(std::collections::HashMap::new(), |mut acc, (bn, mod_)| {
                acc.entry(mod_).or_default().push(bn);
                acc
            });
        let root = root.clone();
        let (signature_pairs, attr_pairs) = tokio::task::spawn_blocking(move || {
            let mut guard = match cache.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            let mut signatures: Vec<(String, semantic::CalleeSignature)> = Vec::new();
            let mut attrs: Vec<(String, String)> = Vec::new();
            for (module, members_wanted) in by_module {
                let Some(members) = guard.members(&root, &module) else {
                    continue;
                };
                for (binding_name, member_name) in members_wanted {
                    let Some(m) = members.iter().find(|m| m.name == member_name) else {
                        continue;
                    };
                    if let Some(sig) = m.signature.as_deref() {
                        signatures.push((binding_name.clone(), semantic::parse_signature(sig)));
                    }
                    // `from M import N` exposes N directly; future
                    // chained attribute access (`N.something`) would
                    // need a second introspection on N, which we
                    // don't do here. Recording the kind for N alone
                    // is unused by the attribute walker (which only
                    // keys `<receiver>.<attr>` shapes), so we skip
                    // the entry for the named case.
                }
            }
            // Bare imports: emit `binding.Foo` keys so attribute
            // callees (`agent_framework.Agent(client=…)`) can match
            // the same lookup path, AND record each member's kind so
            // the semantic-token pass can paint `nn.Module` as a
            // class instead of a generic property.
            for (module, binding_names) in bare_by_module {
                let Some(members) = guard.members(&root, &module) else {
                    continue;
                };
                for m in members.iter() {
                    let parsed = m.signature.as_deref().map(semantic::parse_signature);
                    for binding_name in &binding_names {
                        let key = format!("{}.{}", binding_name, m.name);
                        if let Some(sig) = &parsed {
                            signatures.push((key.clone(), sig.clone()));
                        }
                        attrs.push((key, m.kind.clone()));
                    }
                }
            }
            (signatures, attrs)
        })
        .await
        .unwrap_or_default();
        for (name, sig) in signature_pairs {
            out.insert(name, sig);
        }
        for (key, kind) in attr_pairs {
            attr_kinds.insert(key, kind);
        }
        (out, attr_kinds)
    }
}

impl Backend {
    /// Look up the most recent raw Typhon source text for `uri`.  Returns
    /// `None` when the editor has not yet opened the file or it was closed.
    /// Text is read back from the Salsa database rather than a duplicate store.
    async fn document_text(&self, uri: &Uri) -> Option<String> {
        let sf = {
            let docs = self.documents.lock().await;
            docs.get(uri.as_str()).copied()?
        };
        let db = self.db.lock().await;
        Some(sf.text(&*db).clone())
    }

    /// Return the [`SourceFile`] Salsa handle for `uri`, or `None` when the
    /// file has not been opened.
    async fn source_file_for(&self, uri: &Uri) -> Option<SourceFile> {
        let docs = self.documents.lock().await;
        docs.get(uri.as_str()).copied()
    }

    /// Render the import-specific addition to a hover body: the
    /// declared module path, the recovered signature (in a fenced
    /// `python` code block), and the runtime docstring. Returns
    /// `None` when the symbol isn't an import, when no `typhon.toml`
    /// ancestor anchors a venv, or when introspection failed (no
    /// Python on PATH, import-time error, timeout) — the hover falls
    /// back to the base body in those cases.
    ///
    /// The lookup runs the same introspection cache that powers
    /// completion (`venv_introspect`), so the data is shared and the
    /// per-module subprocess fires at most once per session.
    /// `check_and_publish` pre-warms the cache for every third-party
    /// import in the open document, so the typical hover hits a
    /// fully-populated entry without spawning a subprocess.
    async fn hover_import_extras(
        &self,
        uri: &Uri,
        symbol: &tyc_resolve::SymbolAtOffset<'_>,
    ) -> Option<String> {
        let def = symbol.definition?;
        let import_info = def.import_info.as_ref()?;
        let (root, cache) = self.introspection_cache_for(uri).await?;
        // Run the cache lookup off the async runtime. The lookup can
        // shell to `python` on a cold miss; doing it on the LSP's
        // single-threaded runtime would stall every other request.
        let module = import_info.module.clone();
        let member_name = import_info.member.clone();
        let root_for_task = root.clone();
        let lookup_result: (
            Option<Arc<Vec<venv_introspect::MemberInfo>>>,
            Option<venv_introspect::IntrospectionFailure>,
            Option<std::path::PathBuf>,
        ) = tokio::task::spawn_blocking(move || {
            // Recover from a poisoned mutex rather than silently
            // disabling hover extras for the rest of the session.
            // A poisoned cache still has valid `MemberInfo` entries
            // from any prior successful introspection; the
            // completion path uses the same recovery shape.
            let mut guard = match cache.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            let members = guard.members(&root_for_task, &module);
            let failure = guard.last_failure(&module).cloned();
            let python = guard.python_bin().map(|p| p.to_path_buf());
            (members, failure, python)
        })
        .await
        .ok()?;
        let (members, failure, python_bin) = lookup_result;
        let target_name = member_name.as_deref().unwrap_or("");
        let mut out = String::new();
        out.push_str(&format!("📦 from `{}`", import_info.module));
        if !target_name.is_empty() {
            if let Some(member) = members
                .as_ref()
                .and_then(|ms| ms.iter().find(|m| m.name == target_name))
            {
                let kind_label = match member.kind.as_str() {
                    "class" => "class",
                    "function" => "function",
                    "module" => "submodule",
                    _ => "value",
                };
                out.push_str(&format!(" — *{}*", kind_label));
                if let Some(sig) = &member.signature {
                    out.push_str(&format!(
                        "\n\n```python\n{}{}\n```",
                        target_name,
                        sig_tail(sig, target_name)
                    ));
                }
                if let Some(bases) = member.bases.as_ref().filter(|b| !b.is_empty()) {
                    out.push_str("\n\n*inherits:* ");
                    for (i, base) in bases.iter().enumerate() {
                        if i > 0 {
                            out.push_str(" → ");
                        }
                        out.push('`');
                        out.push_str(base);
                        out.push('`');
                    }
                }
                if let Some(methods) = member.methods.as_ref().filter(|m| !m.is_empty()) {
                    out.push_str("\n\n*methods:* ");
                    for (i, m) in methods.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        out.push('`');
                        out.push_str(m);
                        out.push_str("()`");
                    }
                }
                if let Some(doc) = &member.documentation {
                    out.push_str("\n\n");
                    out.push_str(&render_docstring(doc));
                }
            } else if let Some(reason) =
                render_introspection_failure(&failure, python_bin.as_deref(), &import_info.module)
            {
                out.push_str("\n\n");
                out.push_str(&reason);
            }
        } else if let Some(reason) =
            render_introspection_failure(&failure, python_bin.as_deref(), &import_info.module)
        {
            // Bare `import torch` — no member to look up, but if the
            // module itself failed to introspect we still surface the
            // hint so the user can fix the install / venv before
            // they get to completion.
            out.push_str("\n\n");
            out.push_str(&reason);
        }
        Some(out)
    }

    /// Locate `uri`'s project root (walking upward for `typhon.toml`)
    /// and return its [`venv_introspect::IntrospectionCache`], creating
    /// one on first access. Returns `None` when the URI isn't a local
    /// `file://` path or no `typhon.toml` ancestor exists.
    ///
    /// One cache per project root keeps different workspaces isolated:
    /// project A's `requests` package shouldn't appear as a completion
    /// when project B (which doesn't depend on it) is active.
    async fn introspection_cache_for(
        &self,
        uri: &Uri,
    ) -> Option<(
        std::path::PathBuf,
        Arc<std::sync::Mutex<venv_introspect::IntrospectionCache>>,
    )> {
        // We only know how to introspect when the editor is operating
        // on a real path. `file:///` URIs convert; anything else
        // (`untitled:` / `inmemory:`) doesn't have a venv to point at.
        let path = uri_to_path(uri)?;
        let root = venv_introspect::find_project_root(&path)?;
        let mut guard = self.introspection.lock().await;
        let cache = guard
            .entry(root.clone())
            .or_insert_with(|| {
                Arc::new(std::sync::Mutex::new(
                    venv_introspect::IntrospectionCache::for_project_root(&root),
                ))
            })
            .clone();
        Some((root, cache))
    }

    /// Return the auto-import index for `uri`'s project root, plus the
    /// `src` directory the index should scan. Creates an empty index
    /// on first access; subsequent calls reuse the cached entry so
    /// refresh sees the per-file mtime state from earlier runs.
    /// Returns `None` when `uri` isn't a real file path or no
    /// `typhon.toml` ancestor exists.
    async fn project_index_for(
        &self,
        uri: &Uri,
    ) -> Option<(std::path::PathBuf, Arc<std::sync::Mutex<ProjectIndex>>)> {
        let path = uri_to_path(uri)?;
        let (root, src_dir) = find_workspace_layout(&path)?;
        let mut guard = self.project_indexes.lock().await;
        let index = guard
            .entry(root)
            .or_insert_with(|| Arc::new(std::sync::Mutex::new(ProjectIndex::default())))
            .clone();
        Some((src_dir, index))
    }

    /// Return a [`ResolvedModule`] for a cross-file target URI, caching the
    /// result so repeated go-to-definition jumps into the same module skip
    /// parse + resolve.  Only used for files that are not open in the editor
    /// (same-file operations use the Salsa `resolved_module` query instead).
    ///
    /// `raw_class_byte_starts` lets the resolver tag `class!` declarations
    /// in the cross-file target so hover renders the raw-class marker even
    /// after a go-to-definition jump.
    async fn get_or_resolve(
        &self,
        uri_str: &str,
        preprocessed: &str,
        raw_class_byte_starts: Vec<u32>,
    ) -> Option<Arc<ResolvedModule>> {
        // Fast path: check the cache under a short lock.
        {
            let cache = self.resolved_cache.lock().await;
            if let Some((cached_prep, cached_module)) = cache.get(uri_str) {
                if cached_prep.as_str() == preprocessed {
                    return Some(Arc::clone(cached_module));
                }
            }
        }
        // Slow path: parse and resolve, then store. The LSP doesn't
        // surface unused-import diagnostics on a per-keystroke basis, so
        // the lazy-import remap path stays inert here (empty list +
        // None original source); the disk-backed `tyc check` carries
        // the user-friendly FINDINGS #15 remap.
        let options = ResolveOptions {
            raw_class_byte_starts,
            lazy_import_remaps: Vec::new(),
            original_source: None,
        };
        let resolved = resolve_in_preprocessed(preprocessed, options)?;
        let arc = Arc::new(resolved);
        {
            let mut cache = self.resolved_cache.lock().await;
            cache.insert(
                uri_str.to_owned(),
                (preprocessed.to_owned(), Arc::clone(&arc)),
            );
        }
        Some(arc)
    }

    /// Evict the cross-file cache entry for `uri`.  Called on `did_close` so
    /// stale entries from cross-file jumps don't linger in memory.
    async fn evict_resolved_cache(&self, uri_str: &str) {
        let mut cache = self.resolved_cache.lock().await;
        cache.remove(uri_str);
    }

    /// Resolve an import binding to a `Location` in the originating `.ty`
    /// file, if it can be found on disk.
    ///
    /// Resolution walks up from the current file looking for a `typhon.toml`
    /// to find the workspace root; the configured `src` directory then
    /// anchors the dotted-module lookup.  Returns `None` for stdlib /
    /// third-party imports — those have no `.ty` source to jump to.
    ///
    /// The returned [`Location`] is mapped back to *original* `.ty`
    /// offsets so the LSP client lands on the same column the user sees,
    /// not the preprocessed offset (which would be off by `len("let ")`
    /// for a `let`/`mut` declaration).  The resolved module is cached in
    /// `resolved_cache` keyed by file:// URI so subsequent requests for
    /// the same target skip the parse + resolve work.
    async fn resolve_cross_file_import(
        &self,
        current: &Uri,
        info: &ImportInfo,
    ) -> Option<Location> {
        let current_path = uri_to_path(current)?;
        let (_project_root, src_dir) = find_workspace_layout(&current_path)?;
        let module_path = resolve_module_to_file(&src_dir, &info.module)?;
        let original_source = std::fs::read_to_string(&module_path).ok()?;

        // Cache key: the canonicalised file:// URI of the target file.
        // We store `(preprocessed_text, ResolvedModule)` exactly like the
        // primary did-open path so repeated jumps into the same module
        // skip parse + resolve.
        let target_uri = path_to_uri(&module_path)?;
        let target_uri_str = target_uri.as_str().to_owned();

        let prep = tyc_syntax::preprocess::preprocess(&original_source);
        let raw_class_byte_starts =
            tyc_syntax::preprocess::line_byte_starts(&prep.python_source, &prep.raw_class_lines);
        let resolved = match self
            .get_or_resolve(&target_uri_str, &prep.python_source, raw_class_byte_starts)
            .await
        {
            Some(r) => r,
            None => return None,
        };

        let target_name = info.member.clone();
        let module_scope = 0;
        let target_binding = match target_name {
            Some(name) => resolved
                .scopes
                .get(module_scope)?
                .bindings
                .iter()
                .find(|b| b.name == name),
            None => None,
        };

        let (start_prep, end_prep) = if let Some(b) = target_binding {
            (b.span.0, b.span.1)
        } else {
            (0, 0)
        };
        // Map preprocessed-source offsets back to original-source offsets
        // by adding back the bytes preprocess stripped from earlier lines
        // (each `let ` or `mut ` removed 4 chars from a single line).
        let (start, end) = (
            map_preprocessed_offset_to_original(&prep, &original_source, start_prep),
            map_preprocessed_offset_to_original(&prep, &original_source, end_prep),
        );
        Some(Location {
            uri: target_uri,
            range: Range {
                start: byte_to_position(&original_source, start),
                end: byte_to_position(&original_source, end),
            },
        })
    }
}

/// Map a byte offset in `preprocessed` text back to a byte offset in
/// `original`.  Preprocessing only strips characters from the start of a
/// line (`let ` / `mut `), and lines are not added or removed, so the
/// mapping per line is "original_line_start + (preprocessed_col + stripped_prefix_len)".
///
/// For lines that don't have a stripped prefix the mapping is identity.
fn map_preprocessed_offset_to_original(
    prep: &tyc_syntax::preprocess::PreprocessResult,
    original: &str,
    prep_offset: usize,
) -> usize {
    let prep_text = prep.python_source.as_str();
    // Walk both strings line-by-line, finding which line `prep_offset`
    // falls into and the column within that line.
    let mut line_idx = 0usize;
    let mut prep_line_start = 0usize;
    while prep_line_start < prep_text.len() {
        let line_end = prep_text[prep_line_start..]
            .find('\n')
            .map(|i| prep_line_start + i + 1)
            .unwrap_or(prep_text.len());
        if prep_offset < line_end {
            break;
        }
        prep_line_start = line_end;
        line_idx += 1;
    }
    let prep_col = prep_offset.saturating_sub(prep_line_start);

    // Find the same line in the original text.
    let mut orig_line_start = 0usize;
    for _ in 0..line_idx {
        let Some(i) = original[orig_line_start..].find('\n') else {
            return original.len();
        };
        orig_line_start += i + 1;
    }

    // How many bytes did preprocess strip from the start of this line?
    // Each `let `/`mut ` removed 4 chars; other stripped keywords (impl,
    // extend, …) become wider lowering forms instead of getting trimmed,
    // so they don't shift offsets.
    let stripped_prefix: usize = prep
        .stripped
        .iter()
        .filter(|s| s.line_index == line_idx)
        .map(|s| match s.keyword {
            tyc_syntax::lexer::TyphonKeyword::Let | tyc_syntax::lexer::TyphonKeyword::Mut => 4,
            _ => 0,
        })
        .sum();

    (orig_line_start + prep_col + stripped_prefix).min(original.len())
}

/// Convert an `lsp_types::Uri` into a local filesystem path.  Only `file:`
/// URIs are supported — anything else (e.g. `untitled:`) returns `None`.
fn uri_to_path(uri: &Uri) -> Option<std::path::PathBuf> {
    let s = uri.as_str();
    let stripped = s.strip_prefix("file://")?;
    // Strip the host component (always empty for local files but the LSP
    // URI may emit it as "//"). Anything past the first `/` is the path.
    let path = if let Some(rest) = stripped.strip_prefix('/') {
        format!("/{rest}")
    } else {
        stripped.to_owned()
    };
    Some(std::path::PathBuf::from(percent_decode(&path)))
}

/// Convert a local filesystem path back into an `lsp_types::Uri`.
fn path_to_uri(path: &std::path::Path) -> Option<Uri> {
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let s = abs.to_str()?;
    let encoded = percent_encode(s);
    Uri::from_str(&format!("file://{encoded}")).ok()
}

/// Minimal RFC 3986 percent-encoder for file paths. Encodes everything that
/// isn't an ASCII alphanumeric, `/`, `.`, `-`, or `_` so spaces, `?`, etc.
/// survive the round-trip without confusing the LSP client.
fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        let safe = b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'-' | b'_' | b'~');
        if safe {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Minimal percent decoder — inverse of [`percent_encode`].
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (from_hex(bytes[i + 1]), from_hex(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|e| {
        // Invalid UTF-8 means the URI was corrupted upstream; fall back to
        // the lossy decode rather than panic so the LSP request still
        // returns a (possibly imprecise) answer.
        String::from_utf8_lossy(&e.into_bytes()).into_owned()
    })
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Salsa-backed project shape registry builder.
///
/// Walks every `.ty` / `.dty` file under `src_dir`, ensures each one
/// is registered as a [`SourceFile`] input in the Salsa database, and
/// then queries [`module_shapes_query`] for each. Salsa's input
/// equality check means re-uploading on-disk text that hasn't changed
/// is a no-op; the per-file `module_shapes_query` cache then returns
/// immediately. Net result: a keystroke in `main.ty` only triggers
/// shape extraction for `main.ty`, not every sibling.
///
/// `current_uri` + `current_text` carry the in-flight editor buffer
/// for the document currently being checked. That text is uploaded
/// via `set_text` (or used to create a fresh `SourceFile`) so
/// cross-module diagnostics react to unsaved changes within one
/// keystroke.
///
/// `project_files` is the per-project handle table that survives
/// across calls — without it we'd create a new `SourceFile` on every
/// keystroke and the per-file salsa cache would never hit.
///
/// `.dty` stubs are registered first; the second pass over `.ty`
/// files skips dotted names already in the map so authored stubs
/// remain the authoritative surface for any module.
fn build_project_shapes_salsa(
    db: &mut TycDatabase,
    project_files: &Arc<Mutex<HashMap<std::path::PathBuf, HashMap<String, SourceFile>>>>,
    src_dir: &std::path::Path,
    src_root_name: &str,
    current_uri: &str,
    current_text: &str,
) -> std::collections::HashMap<String, ModuleShapes> {
    let project_root = src_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| src_dir.to_path_buf());

    let mut shapes: std::collections::HashMap<String, ModuleShapes> =
        std::collections::HashMap::new();

    let mut per_project = project_files.blocking_lock();
    let entries = per_project.entry(project_root.clone()).or_default();

    // `.dty` stubs first, so `.ty` insertions skip them on
    // collisions — authored stubs are the source of truth.
    let dty_files = collect_files_with_ext(src_dir, "dty");
    for file in &dty_files {
        let dotted = path_to_dotted(file, src_root_name);
        if shapes.contains_key(&dotted) {
            continue;
        }
        // Prefer the editor buffer for the currently-edited file,
        // disk for everything else. Skip files we can't read.
        let text = if uri_matches_path(current_uri, file) {
            current_text.to_owned()
        } else {
            match std::fs::read_to_string(file) {
                Ok(t) => t,
                Err(_) => continue,
            }
        };
        let source_file = upsert_source_file(db, entries, &dotted, file, text);
        shapes.insert(dotted, (*module_shapes_query(db, source_file).0).clone());
    }

    let ty_files = collect_files_with_ext(src_dir, "ty");
    for file in &ty_files {
        let dotted = path_to_dotted(file, src_root_name);
        if shapes.contains_key(&dotted) {
            continue;
        }
        let text = if uri_matches_path(current_uri, file) {
            current_text.to_owned()
        } else {
            match std::fs::read_to_string(file) {
                Ok(t) => t,
                Err(_) => continue,
            }
        };
        let source_file = upsert_source_file(db, entries, &dotted, file, text);
        shapes.insert(dotted, (*module_shapes_query(db, source_file).0).clone());
    }

    shapes
}

/// Locate or create the Salsa `SourceFile` handle for a project
/// module. If we already have a handle, push the new text through
/// `set_text` (a no-op when content matches); otherwise allocate one
/// via `SourceFile::new`. Either way, returns a handle that
/// `module_shapes_query` can consume.
fn upsert_source_file(
    db: &mut TycDatabase,
    entries: &mut HashMap<String, SourceFile>,
    dotted: &str,
    path: &std::path::Path,
    text: String,
) -> SourceFile {
    if let Some(&sf) = entries.get(dotted) {
        sf.set_text(db).to(text);
        sf
    } else {
        let sf = SourceFile::new(db, path.display().to_string(), text);
        entries.insert(dotted.to_owned(), sf);
        sf
    }
}

/// Recursive file collection that mirrors the CLI's
/// `collect_with_ext` — copied here so this crate stays free of a
/// reverse dependency on the CLI binary crate.
fn collect_files_with_ext(root: &std::path::Path, ext: &str) -> Vec<std::path::PathBuf> {
    let mut acc = Vec::new();
    collect_files_inner(root, ext, &mut acc);
    acc.sort();
    acc
}

fn collect_files_inner(root: &std::path::Path, ext: &str, acc: &mut Vec<std::path::PathBuf>) {
    if root.is_file() {
        if root.extension().and_then(|e| e.to_str()) == Some(ext) {
            acc.push(root.to_path_buf());
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with('.') || name == "__pycache__" {
                    continue;
                }
            }
        }
        collect_files_inner(&path, ext, acc);
    }
}

/// Map a file path to its dotted Python module name. Identical
/// semantics to `tyc::commands::util::path_to_dotted` (kept private
/// here so the LSP crate doesn't need to depend on the CLI binary
/// crate).
fn path_to_dotted(path: &std::path::Path, src_root: &str) -> String {
    let components: Vec<String> = path
        .with_extension("")
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(os) => Some(os.to_string_lossy().to_string()),
            _ => None,
        })
        .collect();
    let src_idx = components.iter().rposition(|c| c == src_root);
    let tail: Vec<&str> = match src_idx {
        Some(i) => components[i + 1..].iter().map(|s| s.as_str()).collect(),
        None => components
            .last()
            .map(|s| vec![s.as_str()])
            .unwrap_or_default(),
    };
    let mut tail = tail;
    if tail.last().is_some_and(|s| *s == "__init__") {
        tail.pop();
    }
    tail.join(".")
}

/// Best-effort check that an editor URI refers to the same file as
/// `path`. Compares canonicalised forms so symlinks and `..` segments
/// don't fool the equality check; falls back to suffix matching when
/// canonicalisation fails (paths that don't exist yet, network FS).
fn uri_matches_path(uri: &str, path: &std::path::Path) -> bool {
    let Some(uri_path_str) = uri.strip_prefix("file://") else {
        return false;
    };
    let uri_path = std::path::PathBuf::from(uri_path_str);
    match (uri_path.canonicalize(), path.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => uri_path == path,
    }
}

/// Walk up from `file_path` looking for a `typhon.toml`.  Returns
/// `(project_root, src_dir)` — `src_dir` defaults to `project_root/src`
/// when the toml does not specify, matching `tyc init`'s scaffolding.
fn find_workspace_layout(
    file_path: &std::path::Path,
) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let mut dir = file_path.parent()?.to_path_buf();
    loop {
        let candidate = dir.join("typhon.toml");
        if candidate.exists() {
            let src = parse_src_dir(&candidate).unwrap_or_else(|| "src".to_owned());
            let src_dir = dir.join(src);
            return Some((dir, src_dir));
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Pull out the `[project] src` field from `typhon.toml`.
///
/// Uses the `toml` crate so inline-table values, end-of-line comments,
/// nested tables, and the array-of-tables syntax are handled correctly
/// (the previous line-by-line scanner choked on any of those).  Returns
/// `None` when the file is unreadable, malformed, or doesn't carry a
/// `[project] src = "…"` entry — callers fall through to the default
/// `"src"` directory in that case.
fn parse_src_dir(toml_path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(toml_path).ok()?;
    let parsed: toml::Value = toml::from_str(&text).ok()?;
    let project = parsed.get("project")?.as_table()?;
    let src = project.get("src")?.as_str()?;
    Some(src.to_owned())
}

/// Map a dotted module name to a `.ty` file path under `src_dir`.
///
/// `pkg.util` → `src_dir/pkg/util.ty`, falling back to
/// `src_dir/pkg/util/__init__.ty` when the leaf is itself a package.
/// Returns `None` when neither candidate exists on disk.
fn resolve_module_to_file(src_dir: &std::path::Path, module: &str) -> Option<std::path::PathBuf> {
    let parts: Vec<&str> = module.split('.').collect();
    if parts.is_empty() {
        return None;
    }
    let mut leaf = src_dir.to_path_buf();
    for (i, segment) in parts.iter().enumerate() {
        if i + 1 == parts.len() {
            let direct = leaf.join(format!("{segment}.ty"));
            if direct.exists() {
                return Some(direct);
            }
            let pkg_init = leaf.join(segment).join("__init__.ty");
            if pkg_init.exists() {
                return Some(pkg_init);
            }
            return None;
        } else {
            leaf = leaf.join(segment);
        }
    }
    None
}

use std::str::FromStr;

/// Parse and resolve a pre-preprocessed Python source string so hover and
/// go-to-definition can query bindings and references.  Returns `None` when
/// the source fails to parse.
///
/// `options` carries Typhon-specific metadata the parser cannot recover from
/// the preprocessed text (e.g. which `class` declarations were originally
/// written `class!`).
fn resolve_in_preprocessed(preprocessed: &str, options: ResolveOptions) -> Option<ResolvedModule> {
    let parsed = tyc_syntax::parse_module(preprocessed).ok()?;
    let module = parsed.into_syntax();
    let (resolved, _) =
        tyc_resolve::resolve_module_with("<lsp>".to_owned(), preprocessed, &module, options);
    Some(resolved)
}

/// Look up a dotted module name against the project's `src` directory
/// and return its top-level public bindings as completion items.
///
/// Reuses [`resolve_module_to_file`] for the `.ty` / `__init__.ty`
/// search and the standard preprocess + parse + resolve pipeline for
/// the file's contents. Returns `None` when the module doesn't map to
/// a file we can read, parse, and resolve.
///
/// Used by the LSP completion handler to surface cross-file imports —
/// typing `from utils import <cursor>` against the user's own
/// `src/utils.ty` now produces the same menu as `import os` would.
/// Underscore-prefixed names and `import`-bound re-exports are
/// excluded; only the symbols the file genuinely defines surface.
fn project_module_members(src_dir: &std::path::Path, module: &str) -> Option<Vec<CompletionItem>> {
    let file = resolve_module_to_file(src_dir, module)?;
    let source = std::fs::read_to_string(&file).ok()?;
    let prep = tyc_syntax::preprocess::preprocess(&source);
    let raw_class_byte_starts =
        tyc_syntax::preprocess::line_byte_starts(&prep.python_source, &prep.raw_class_lines);
    let resolved = resolve_in_preprocessed(
        &prep.python_source,
        ResolveOptions {
            raw_class_byte_starts,
            lazy_import_remaps: Vec::new(),
            original_source: None,
        },
    )?;
    let module_scope = resolved.scopes.first()?;
    let items: Vec<CompletionItem> = module_scope
        .bindings
        .iter()
        .filter(|b| !b.name.starts_with('_'))
        // Drop bindings introduced by `import` statements: surfacing
        // them would mean re-exporting every helper the module
        // imports, which isn't what the user means by "show me what
        // this module exports."
        .filter(|b| b.kind != BindingKind::Import)
        .map(binding_to_completion)
        .collect();
    Some(items)
}

/// One entry in the project-wide symbol index: a top-level public
/// binding declared in some `.ty` file of the user's project, along
/// with the dotted module path it lives in. Used to drive auto-import
/// suggestions on open-completion.
#[derive(Clone, Debug)]
struct AutoImportEntry {
    module: String,
    kind: BindingKind,
}

/// Cached per-file index state. Keyed in [`ProjectIndex::files`] by
/// the file's absolute path. The `(mtime, len)` stamp lets refresh
/// skip parse + resolve work when the file hasn't changed — pairing
/// length with mtime catches edits that land within a single
/// filesystem tick (some macOS / Linux filesystems only have
/// 1-second mtime resolution, so a fast save sequence can keep the
/// same timestamp).
#[derive(Debug)]
struct IndexedFile {
    mtime: std::time::SystemTime,
    len: u64,
    module: String,
    symbols: Vec<(String, BindingKind)>,
}

/// Per-project index of top-level public symbols across every `.ty`
/// file under `src_dir`. Drives auto-import suggestions on
/// open-completion: typing `Agent<Ctrl+Space>` looks up `Agent` in
/// `by_name`, and each matching `(module, kind)` becomes a
/// `CompletionItem` whose `additionalTextEdits` insert
/// `from <module> import Agent` at the top of the current file.
///
/// The index is *lazy*: `refresh` runs on demand from the completion
/// handler, statting every `.ty` file once per call and re-parsing
/// only files whose mtime advanced. Empty projects + steady-state
/// editing are cheap (just stat calls); a cold cache pays one parse
/// per file.
#[derive(Debug, Default)]
struct ProjectIndex {
    files: HashMap<std::path::PathBuf, IndexedFile>,
    by_name: HashMap<String, Vec<AutoImportEntry>>,
}

impl ProjectIndex {
    /// Walk `src_dir` for `.ty` files, refresh changed entries, drop
    /// stale ones, and rebuild [`by_name`] when anything changed.
    fn refresh(&mut self, src_dir: &std::path::Path) {
        let live = collect_ty_files(src_dir);
        let mut dirty = false;

        // Drop entries whose files no longer exist.
        let stale: Vec<std::path::PathBuf> = self
            .files
            .keys()
            .filter(|p| !live.contains(*p))
            .cloned()
            .collect();
        for p in stale {
            self.files.remove(&p);
            dirty = true;
        }

        // Add or refresh entries.
        for path in live {
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let Ok(mtime) = meta.modified() else { continue };
            let len = meta.len();
            let unchanged = self
                .files
                .get(&path)
                .is_some_and(|entry| entry.mtime == mtime && entry.len == len);
            if unchanged {
                continue;
            }
            let Some(module) = compute_module_path(src_dir, &path) else {
                continue;
            };
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            let symbols = extract_top_level_publics(&source);
            self.files.insert(
                path,
                IndexedFile {
                    mtime,
                    len,
                    module,
                    symbols,
                },
            );
            dirty = true;
        }

        if dirty {
            self.rebuild_by_name();
        }
    }

    fn rebuild_by_name(&mut self) {
        self.by_name.clear();
        for entry in self.files.values() {
            for (name, kind) in &entry.symbols {
                self.by_name
                    .entry(name.clone())
                    .or_default()
                    .push(AutoImportEntry {
                        module: entry.module.clone(),
                        kind: *kind,
                    });
            }
        }
    }
}

/// Recursively collect every `.ty` file under `dir`, skipping common
/// vendor directories (`.venv`, `node_modules`, dot-prefixed hidden
/// folders) so we don't accidentally parse the user's installed
/// dependencies.
fn collect_ty_files(dir: &std::path::Path) -> std::collections::HashSet<std::path::PathBuf> {
    let mut out = std::collections::HashSet::new();
    walk_ty(dir, &mut out);
    out
}

fn walk_ty(dir: &std::path::Path, out: &mut std::collections::HashSet<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            let skip = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.') || n == "node_modules" || n == "target");
            if !skip {
                walk_ty(&path, out);
            }
        } else if file_type.is_file() && path.extension().and_then(|e| e.to_str()) == Some("ty") {
            out.insert(path);
        }
    }
}

/// Convert a `.ty` file path under `src_dir` into the dotted module
/// path that imports would use. `src/tools/web_tools.ty` →
/// `"tools.web_tools"`; `src/pkg/__init__.ty` → `"pkg"` (the
/// `__init__` segment is implicit, matching Python's package model).
fn compute_module_path(src_dir: &std::path::Path, file: &std::path::Path) -> Option<String> {
    let rel = file.strip_prefix(src_dir).ok()?;
    let no_ext = rel.with_extension("");
    let mut parts: Vec<String> = no_ext
        .iter()
        .filter_map(|c| c.to_str().map(|s| s.to_owned()))
        .collect();
    if parts.last().map(String::as_str) == Some("__init__") {
        parts.pop();
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("."))
}

/// Parse + resolve `source` and return its top-level public symbols
/// suitable for the project index. Underscore-prefixed names and
/// `import`-bound re-exports are filtered, matching the from-import
/// completion path's notion of "what this module exports".
fn extract_top_level_publics(source: &str) -> Vec<(String, BindingKind)> {
    let prep = tyc_syntax::preprocess::preprocess(source);
    let raw_class_byte_starts =
        tyc_syntax::preprocess::line_byte_starts(&prep.python_source, &prep.raw_class_lines);
    let Some(resolved) = resolve_in_preprocessed(
        &prep.python_source,
        ResolveOptions {
            raw_class_byte_starts,
            lazy_import_remaps: Vec::new(),
            original_source: None,
        },
    ) else {
        return Vec::new();
    };
    let Some(module_scope) = resolved.scopes.first() else {
        return Vec::new();
    };
    module_scope
        .bindings
        .iter()
        .filter(|b| !b.name.starts_with('_'))
        .filter(|b| b.kind != BindingKind::Import)
        .map(|b| (b.name.clone(), b.kind))
        .collect()
}

/// Compute the (zero-width) insertion [`Range`] for an auto-import
/// statement in `raw_source`. Lands after the last top-level `import`
/// / `from … import …` statement; falls through to a position past
/// any leading shebang / module docstring when no imports exist yet.
///
/// Indented imports (e.g. `import foo` inside a function body) are
/// ignored — anchoring on those would let the auto-import drop a
/// top-level statement into the middle of a block, producing
/// syntactically broken code.
///
/// Doesn't try to merge into an existing `from <module> import …` for
/// the same module — that would need real lexer state to handle
/// parenthesised multi-line forms safely. The duplicate import that
/// results from picking auto-import twice for the same name is
/// already surfaced by `tyc check` as a `duplicate_binding` error, so
/// the user sees the redundancy immediately.
fn auto_import_insertion_range(raw_source: &str) -> Range {
    let lines: Vec<&str> = raw_source.lines().collect();
    let insert_line = scan_last_top_level_import(&lines)
        .unwrap_or_else(|| scan_past_shebang_and_docstring(&lines));
    let pos = Position {
        line: insert_line,
        character: 0,
    };
    Range {
        start: pos,
        end: pos,
    }
}

/// Build the `additionalTextEdits` payload for a single auto-import
/// suggestion. The expensive line-scan happens once per completion
/// request (in [`auto_import_insertion_range`]); this just stamps the
/// resulting `Range` with the per-symbol `from <module> import <name>`
/// text so the cost is O(symbols) instead of O(symbols × lines).
fn auto_import_text_edit_at(range: Range, module: &str, name: &str) -> TextEdit {
    TextEdit {
        range,
        new_text: format!("from {module} import {name}\n"),
    }
}

/// Return the line index *after* the last top-level `import` /
/// `from … import …` statement, or `None` when the file has no
/// top-level imports. Lines that begin with whitespace before the
/// keyword are nested inside another block and don't count.
fn scan_last_top_level_import(lines: &[&str]) -> Option<u32> {
    let mut last: Option<u32> = None;
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("import ") || line.starts_with("from ") {
            last = Some((i + 1) as u32);
        }
    }
    last
}

/// Skip a leading `#!` shebang and any module docstring, returning
/// the first line index that's safe to insert a top-level statement
/// at. Conservative: anything we can't classify cleanly stops the
/// scan and the insert lands above it.
fn scan_past_shebang_and_docstring(lines: &[&str]) -> u32 {
    let mut idx: usize = 0;
    if lines.first().is_some_and(|l| l.starts_with("#!")) {
        idx += 1;
    }
    // Skip blank lines / comments between shebang and docstring.
    while let Some(line) = lines.get(idx) {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            idx += 1;
        } else {
            break;
        }
    }
    // Module docstring: triple-quoted string literal at top level.
    // Handles both `"""..."""` and `'''...'''`, single-line and
    // multi-line, with an optional `r`, `b`, or `u` prefix.
    if let Some(line) = lines.get(idx) {
        let trimmed = line.trim_start();
        let stripped = trimmed
            .strip_prefix(|c: char| matches!(c, 'r' | 'R' | 'b' | 'B' | 'u' | 'U'))
            .unwrap_or(trimmed);
        let opener = if stripped.starts_with("\"\"\"") {
            Some("\"\"\"")
        } else if stripped.starts_with("'''") {
            Some("'''")
        } else {
            None
        };
        if let Some(q) = opener {
            // Single-line docstring (closer on the same line, after
            // the opener) — advance one line and we're done.
            let body = &stripped[q.len()..];
            if body.contains(q) {
                idx += 1;
            } else {
                // Multi-line: walk forward until the closer.
                idx += 1;
                while let Some(line) = lines.get(idx) {
                    idx += 1;
                    if line.contains(q) {
                        break;
                    }
                }
            }
        }
    }
    idx as u32
}

fn binding_kind_to_completion_kind(kind: BindingKind) -> CompletionItemKind {
    match kind {
        BindingKind::Function => CompletionItemKind::FUNCTION,
        BindingKind::Class => CompletionItemKind::CLASS,
        BindingKind::Parameter => CompletionItemKind::VARIABLE,
        BindingKind::Import => CompletionItemKind::MODULE,
        BindingKind::Loop => CompletionItemKind::VARIABLE,
        BindingKind::Value => CompletionItemKind::VARIABLE,
    }
}

/// Log-message level for the LSP backend. Maps the user-facing `--log-level`
/// flag onto the subset of severities the `client.log_message` channel
/// supports. `Info` is the default; `Error` suppresses all but errors.
#[derive(Debug, Clone, Copy, Default)]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
}

impl LogLevel {
    /// Parse a `--log-level` argument. Falls back to `Info` for unknown
    /// values so a typo doesn't silently disable status messages.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "error" => LogLevel::Error,
            "warn" | "warning" => LogLevel::Warn,
            "debug" | "trace" => LogLevel::Debug,
            _ => LogLevel::Info,
        }
    }
}

/// Spin up the LSP backend on stdin/stdout. Blocks until the editor sends
/// `exit`. Spawns its own tokio runtime so the caller can stay synchronous.
///
/// `log_level` controls the severity threshold for messages the backend
/// forwards to the editor via `client.log_message`. Messages below the
/// threshold are dropped.
pub fn run_stdio(log_level: LogLevel) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to start tokio runtime for tyc-lsp");

    runtime.block_on(async {
        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        let (service, socket) = LspService::new(move |client| Backend {
            client,
            db: Arc::new(Mutex::new(TycDatabase::new())),
            log_level,
            documents: Arc::new(Mutex::new(HashMap::new())),
            resolved_cache: Arc::new(Mutex::new(HashMap::new())),
            introspection: Arc::new(Mutex::new(HashMap::new())),
            signature_caches: Arc::new(Mutex::new(HashMap::new())),
            project_indexes: Arc::new(Mutex::new(HashMap::new())),
            project_files: Arc::new(Mutex::new(HashMap::new())),
            prewarmed_versions: Arc::new(Mutex::new(HashMap::new())),
        });
        Server::new(stdin, stdout, socket).serve(service).await;
    });
}

/// Typhon-specific keywords that the LSP advertises in completion.  Kept
/// in one place so adding a new keyword (e.g. `gather`, `pure`) updates
/// completion uniformly.
const TYPHON_KEYWORDS: &[&str] = &[
    "let",
    "mut",
    "interface",
    "model",
    "impl",
    "extend",
    "unsafe",
    "comptime",
    "lazy",
    "gather",
    "go",
    "pure",
    "memo",
    // Raw-class modifier: `class! Foo(Base):` skips dataclass injection.
    "class!",
];

/// A short, hand-curated list of Python builtins surfaced in completion so
/// the LSP has something to offer even before any names are declared.  The
/// resolver already accepts these as in-scope (see `builtin_names()` in
/// `tyc-resolve`) so a completion that picks one is always valid.
const COMMON_BUILTINS: &[&str] = &[
    "print",
    "len",
    "range",
    "abs",
    "min",
    "max",
    "sum",
    "sorted",
    "list",
    "dict",
    "set",
    "tuple",
    "int",
    "str",
    "float",
    "bool",
    "bytes",
    "isinstance",
    "issubclass",
    "type",
    "enumerate",
    "zip",
    "map",
    "filter",
    "any",
    "all",
];

/// Build the list of completion items for a cursor in `preprocessed` text
/// at `position`.  Pure of LSP plumbing so it can be unit-tested.
///
/// Two modes:
///
/// 1. **Member access** — when the cursor sits just after a `.` (or
///    after `<identifier>.<prefix>`), [`extract_member_access_receiver`]
///    returns the dotted receiver. We resolve that receiver through the
///    scope chain; if it bottoms out at an `import` binding, the
///    imported module is queried via the optional venv-introspection
///    callback (production wiring) and, on failure, via the curated
///    [`stdlib_stubs`] table.
///    Type-driven completion for builtin-typed `let` bindings
///    (`let xs: list[int] = []; xs.<TAB>` → list methods) is not wired
///    yet; tracked as a follow-up.
/// 2. **Open completion** — the original behaviour: every binding
///    visible from the cursor's scope, every Typhon keyword, and a
///    small set of common Python builtins.
///
/// The LSP client is responsible for prefix-filtering the returned list,
/// so we always return the full menu rather than filtering ourselves.
pub fn compute_completion_items(
    resolved: &ResolvedModule,
    preprocessed: &str,
    position: Position,
) -> Vec<CompletionItem> {
    compute_completion_items_with_introspection(resolved, preprocessed, position, None)
}

/// Variant of [`compute_completion_items`] that accepts a venv-driven
/// introspection callback. When supplied, the callback is consulted
/// first for member-access completions; it receives the resolved
/// dotted module path (e.g. `"os.path"` after walking through
/// `ImportInfo`) and returns the introspected member list. A `None`
/// return signals "module not importable / no venv available" and we
/// fall back to the curated [`stdlib_stubs`] tables.
///
/// The production LSP backend passes a closure that delegates to
/// [`venv_introspect::IntrospectionCache`]; unit tests pass `None` to
/// exercise the curated-stub path deterministically.
pub fn compute_completion_items_with_introspection(
    resolved: &ResolvedModule,
    preprocessed: &str,
    position: Position,
    introspect: Option<&IntrospectFn<'_>>,
) -> Vec<CompletionItem> {
    let offset = position_to_byte(preprocessed, position);
    // Member-access path: detect `receiver.` (with optional partial member
    // name) immediately before the cursor and try to surface stub members.
    if let Some(receiver) = extract_member_access_receiver(preprocessed, offset) {
        if let Some(items) = member_completion_items(resolved, &receiver, offset, introspect) {
            return items;
        }
        // Receiver was an identifier but we couldn't resolve it to a
        // known module / builtin. Returning an empty list is intentional
        // here: emitting the open-completion menu after `.` would just
        // be noise (none of those names are valid members) and confuse
        // the editor's UX. The client falls back to its own filtering
        // on the next keystroke once more context is typed.
        return Vec::new();
    }
    // From-import path: cursor sits inside the import list of a
    // `from <module> import <cursor>` statement. Surface the module's
    // exported members so the user can pick from real names instead of
    // typing them blind. When neither venv introspection nor the
    // curated stubs know the module, return empty — keywords/builtins
    // would be misleading here, they aren't valid as imported names.
    if let Some(module) = extract_from_import_module(preprocessed, offset) {
        return module_member_items(&module, introspect).unwrap_or_default();
    }
    let scope_id = resolved.scope_at_offset(offset);
    let mut items: Vec<CompletionItem> = Vec::new();
    for b in resolved.visible_bindings(scope_id) {
        items.push(binding_to_completion(b));
    }
    for kw in TYPHON_KEYWORDS {
        items.push(simple_completion(
            kw,
            CompletionItemKind::KEYWORD,
            Some("Typhon keyword".to_owned()),
        ));
    }
    for name in COMMON_BUILTINS {
        items.push(simple_completion(
            name,
            CompletionItemKind::FUNCTION,
            Some("builtin".to_owned()),
        ));
    }
    items
}

/// Re-parse `preprocessed` after inserting a placeholder identifier
/// where the cursor sits, returning the patched text + a fresh
/// `ResolvedModule`. Used as a fallback when the cached resolution is
/// empty (typical mid-keystroke state: the source has a trailing `.`
/// and the parser refuses).
///
/// The patch is intentionally minimal: a single byte `X` is appended
/// after a trailing `.` before the cursor when one exists. That turns
/// `os.<cursor>` into `os.X<cursor>` which parses cleanly. We never
/// touch the cursor position — `compute_completion_items` reads from
/// the same offset, and the placeholder appears strictly after it, so
/// receiver extraction still sees `os` as the receiver.
///
/// Returns `None` when no useful fix-up exists (e.g. the cursor is not
/// after a `.`), letting the caller fall back to the empty resolution.
fn try_fixup_and_resolve(
    preprocessed: &str,
    position: Position,
) -> Option<(String, ResolvedModule)> {
    let offset = position_to_byte(preprocessed, position);
    // Cursor must sit immediately after `<id>.` (with possibly some
    // already-typed partial member chars between the dot and the cursor).
    let bytes = preprocessed.as_bytes();
    if offset > bytes.len() {
        return None;
    }
    // Walk left past any in-progress identifier chars.
    let mut i = offset;
    while i > 0 {
        let c = bytes[i - 1];
        if c.is_ascii_alphanumeric() || c == b'_' {
            i -= 1;
        } else {
            break;
        }
    }
    if i == 0 || bytes[i - 1] != b'.' {
        return None;
    }
    // Insert `X` (a stand-in identifier) at byte offset `offset` — i.e.
    // right where the cursor sits. Pushing it strictly *after* the
    // cursor matters: `extract_member_access_receiver` reads from the
    // cursor backwards, so the placeholder doesn't show up in the
    // receiver text.
    let mut patched = String::with_capacity(preprocessed.len() + 1);
    patched.push_str(&preprocessed[..offset]);
    patched.push('X');
    patched.push_str(&preprocessed[offset..]);
    // Re-run preprocess + parse + resolve on the patched source. If any
    // step still fails (broken source elsewhere in the file), bail.
    let prep = tyc_syntax::preprocess::preprocess(&patched);
    let parsed = tyc_syntax::parse_module(&prep.python_source).ok()?;
    let (resolved, _) = tyc_resolve::resolve_module(
        "<lsp-fixup>".to_owned(),
        &prep.python_source,
        parsed.syntax(),
    );
    Some((prep.python_source, resolved))
}

/// Scan backwards from `offset` looking for a `<dotted-name>.<partial>?`
/// pattern and return the dotted name. Returns `None` when the cursor is
/// not in a member-access context.
///
/// We're operating on raw text (not the AST) deliberately: completion
/// fires *while* the user is typing, so the surrounding source very
/// often does not parse. Skipping the AST keeps us robust against
/// in-flight syntax.
///
/// The receiver is allowed to be a multi-segment dotted name (`os.path`,
/// `pkg.sub.mod`) so completion works on submodules too. We don't try to
/// look inside parens, brackets, or string literals — those forms would
/// need real lexer state, and the failure mode (no completion menu) is
/// strictly less bad than guessing wrong.
pub fn extract_member_access_receiver(text: &str, offset: usize) -> Option<String> {
    let bytes = text.as_bytes();
    if offset > bytes.len() {
        return None;
    }
    // Skip any in-progress member name to the left of the cursor — chars
    // that could be part of an identifier. The cursor might sit anywhere
    // from immediately after `.` to several characters into the member
    // name (`os.path.jo|`), so we walk left over identifier characters
    // first and then expect to land on a `.`.
    let mut i = offset;
    while i > 0 {
        let c = bytes[i - 1];
        if c.is_ascii_alphanumeric() || c == b'_' {
            i -= 1;
        } else {
            break;
        }
    }
    if i == 0 {
        return None;
    }
    if bytes[i - 1] != b'.' {
        return None;
    }
    // Now walk left over the dotted receiver. Identifier chars plus `.`
    // are the only allowed glyphs; whitespace, parens, etc. terminate
    // the receiver.
    let end = i - 1; // index of the `.` immediately before the partial member
    let mut start = end;
    while start > 0 {
        let c = bytes[start - 1];
        if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' {
            start -= 1;
        } else {
            break;
        }
    }
    // Trim any trailing `.` from the receiver (shouldn't happen — we
    // anchored on a `.` and `end` points at it — but be defensive). The
    // receiver text spans `[start, end)`.
    if start >= end {
        return None;
    }
    let receiver = &text[start..end];
    // Reject a receiver that's empty after stripping or that starts /
    // ends with `.` — those signal a syntactically incomplete chain
    // (`.foo`, `os..path`) and we can't meaningfully complete them.
    if receiver.is_empty() || receiver.starts_with('.') || receiver.ends_with('.') {
        return None;
    }
    Some(receiver.to_owned())
}

/// Detect that the cursor sits inside the import list of a
/// `from <module> import <cursor>` statement and return `<module>` —
/// the dotted module name whose exported members the editor should
/// surface as completions.
///
/// Returns `None` when the cursor isn't in such a position (regular
/// code, plain `import X`, or a `from X import …` that hasn't reached
/// the `import` keyword yet).
///
/// Like [`extract_member_access_receiver`], this operates on raw text
/// rather than the AST: completion fires while the user is typing and
/// the source very often doesn't parse, so an AST-based detector would
/// miss the most common case. We restrict ourselves to the same line
/// as the cursor for v1; parenthesised multi-line imports
/// (`from x import (\n    <cursor>\n)`) fall through and return `None`.
pub fn extract_from_import_module(text: &str, offset: usize) -> Option<String> {
    if offset > text.len() {
        return None;
    }
    let bytes = text.as_bytes();
    // Find the start of the current logical line.
    let mut line_start = offset;
    while line_start > 0 && bytes[line_start - 1] != b'\n' {
        line_start -= 1;
    }
    let prefix = &text[line_start..offset];
    let trimmed = prefix.trim_start();
    // Must literally start with "from " — we don't try to handle
    // `\timport-as-expression` or other oddities.
    let after_from = trimmed.strip_prefix("from ")?.trim_start();
    // Scan the dotted module name greedily.
    let mut module_end = 0;
    for (i, c) in after_from.char_indices() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
            module_end = i + c.len_utf8();
        } else {
            break;
        }
    }
    if module_end == 0 {
        return None;
    }
    let module = &after_from[..module_end];
    // Module mustn't start or end with `.` — `from .foo` is a relative
    // import we don't introspect, and `from foo.` is mid-typing the
    // module path (the user hasn't reached `import` yet).
    if module.starts_with('.') || module.ends_with('.') {
        return None;
    }
    // After the module there must be whitespace, then `import`, then
    // a separator (space, paren, or end of slice — the partial import
    // list starts here).
    let after_module = &after_from[module_end..];
    let after_ws = after_module.trim_start();
    if after_ws.len() == after_module.len() {
        // No whitespace between module and the next token → still
        // typing the module name.
        return None;
    }
    let rest = after_ws.strip_prefix("import")?;
    // The character following "import" determines whether we're in
    // the import list. We accept the cursor sitting *exactly* at
    // "import|" (empty `rest`) so completion fires as soon as the
    // user finishes typing the keyword.
    match rest.chars().next() {
        None => {}
        Some(c) if c.is_whitespace() || c == '(' => {}
        _ => return None,
    }
    // Guard against the cursor having walked off the end of the
    // import statement on the same line. `from os import path; x = `
    // or `from os import path  # comment ` both literally start with
    // `from … import …` but the cursor is no longer in the import
    // list — `;` ends the statement, `#` starts a comment.  Either
    // means open-code (or comment) completion is the right answer,
    // not module members.
    if rest.contains(';') || rest.contains('#') {
        return None;
    }
    Some(module.to_owned())
}

/// Signature for the per-completion-request introspection callback.
/// Defined as a type alias so the function/handler signatures don't
/// trip clippy's `type_complexity` lint and stay readable.
type IntrospectFn<'a> = dyn Fn(&str) -> Option<Vec<CompletionItem>> + 'a;

/// Resolve `receiver` against the scope visible at `offset` and return
/// the candidate dotted module paths to query for member completion.
/// Each path is tried in order; the first one that produces members
/// wins. Returns an empty `Vec` when no plausible interpretation
/// exists (unknown receiver, non-import binding, etc.).
///
/// Used both at completion time (to feed `member_completion_items`)
/// and at the async-handler boundary (to know which modules to
/// pre-introspect before entering the synchronous completion path).
///
/// Interpretations, in priority order:
///
/// 1. **Dotted module path** — `os.path`, `urllib.parse`: the receiver
///    text *is* the module path. Handles the common case where the
///    user typed the fully-qualified name without aliasing.
/// 2. **Imported alias** — `np` from `import numpy as np`: walk the
///    binding's `ImportInfo` back to the original module name. For
///    dotted receivers (`np.linalg.`), the alias resolves only the
///    head; the trailing segments are appended onto the
///    `ImportInfo.module` path.
fn candidate_module_paths(resolved: &ResolvedModule, receiver: &str, offset: usize) -> Vec<String> {
    let mut out: Vec<String> = vec![receiver.to_owned()];
    let (head, tail) = match receiver.split_once('.') {
        Some((h, t)) => (h, Some(t)),
        None => (receiver, None),
    };
    let scope_id = resolved.scope_at_offset(offset);
    let Some(binding) = resolved.lookup(scope_id, head).map(|(b, _)| b) else {
        return out;
    };
    if binding.kind != BindingKind::Import {
        return out;
    }
    let Some(info) = binding.import_info.as_ref() else {
        return out;
    };
    let module_path = match (info.member.as_deref(), tail) {
        // `import os` + receiver `os.path` → `"os.path"`.
        (None, Some(t)) => format!("{}.{}", info.module, t),
        // `import os` + receiver `os` → `"os"`.
        (None, None) => info.module.clone(),
        // `from os import path` + receiver `path` → `"os.path"`.
        (Some(m), None) => format!("{}.{}", info.module, m),
        // `from os import path` + receiver `path.foo` → `"os.path.foo"`.
        (Some(m), Some(t)) => format!("{}.{}.{}", info.module, m, t),
    };
    if !out.contains(&module_path) {
        out.push(module_path);
    }
    out
}

/// Resolve `receiver` against the scope visible at `offset` and, if it
/// bottoms out at a known import, return the matching stub members.
/// Calls [`candidate_module_paths`] to enumerate interpretations and
/// then delegates to [`module_member_items`] for the introspect/stub
/// lookup applied to each candidate.
fn member_completion_items(
    resolved: &ResolvedModule,
    receiver: &str,
    offset: usize,
    introspect: Option<&IntrospectFn<'_>>,
) -> Option<Vec<CompletionItem>> {
    candidate_module_paths(resolved, receiver, offset)
        .iter()
        .find_map(|module| module_member_items(module, introspect))
}

/// Look up the completion items for a single dotted module path:
/// ask the venv-driven introspection callback first, then fall back
/// to the curated [`stdlib_stubs`] table. Returns `None` when neither
/// source knows the module — letting the caller decide whether to
/// surface a fallback menu or stay quiet.
fn module_member_items(
    module: &str,
    introspect: Option<&IntrospectFn<'_>>,
) -> Option<Vec<CompletionItem>> {
    if let Some(cb) = introspect {
        if let Some(items) = cb(module) {
            return Some(items);
        }
    }
    stdlib_stubs::lookup(module).map(stub_members_to_completion)
}

/// Convert a venv-introspected [`venv_introspect::MemberInfo`] list
/// into LSP `CompletionItem`s, mapping the string `kind` to the LSP
/// enum and skipping any member whose name starts with `_` (the
/// introspect script already drops those, but the second filter
/// here makes the public contract self-contained).
pub(crate) fn introspected_members_to_completion(
    members: &[venv_introspect::MemberInfo],
) -> Vec<CompletionItem> {
    members
        .iter()
        .filter(|m| !m.name.starts_with('_'))
        .map(|m| CompletionItem {
            label: m.name.clone(),
            kind: Some(introspected_kind_to_lsp(&m.kind)),
            detail: m.signature.clone(),
            documentation: m.documentation.clone().map(Documentation::String),
            ..Default::default()
        })
        .collect()
}

/// Map the string kind the Python helper writes (`"function"`,
/// `"class"`, `"module"`, `"value"`) to a `CompletionItemKind` so the
/// editor renders the appropriate icon.
fn introspected_kind_to_lsp(kind: &str) -> CompletionItemKind {
    match kind {
        "class" => CompletionItemKind::CLASS,
        "module" => CompletionItemKind::MODULE,
        "function" => CompletionItemKind::FUNCTION,
        _ => CompletionItemKind::VALUE,
    }
}

/// Convert a slice of curated [`stdlib_stubs::StubMember`] entries into
/// LSP `CompletionItem`s, populating `detail` from the signature line
/// and `documentation` from the one-liner doc when present.
fn stub_members_to_completion(members: &[stdlib_stubs::StubMember]) -> Vec<CompletionItem> {
    members
        .iter()
        .map(|m| CompletionItem {
            label: m.name.to_owned(),
            kind: Some(m.kind),
            detail: m.signature.map(|s| s.to_owned()),
            documentation: m.documentation.map(|d| Documentation::String(d.to_owned())),
            ..Default::default()
        })
        .collect()
}

/// Build the list of code actions for `diagnostics` against `text`.
/// Pure of LSP plumbing so it can be unit-tested.
///
/// v1 surfaces a single action: a `tyc::unused_import` diagnostic gets
/// a "Remove unused import" quick-fix that deletes the offending line,
/// but **only when the line is a single, simple import statement**.
/// Lines that mix multiple imports (`import os, sys`), chain statements
/// with `;`, or otherwise carry content beyond the import are skipped:
/// deleting the whole line would lose the still-used parts.  A
/// follow-up can offer a surgical edit that removes just the dead
/// alias for the comma case.
pub fn compute_code_actions(
    uri: &Uri,
    text: &str,
    diagnostics: &[Diagnostic],
) -> Vec<CodeActionOrCommand> {
    let mut actions: Vec<CodeActionOrCommand> = Vec::new();
    for diag in diagnostics {
        if !diagnostic_code_matches(diag, "tyc::unused_import") {
            continue;
        }
        let line = diag.range.start.line;
        let line_text = nth_line_content(text, line);
        if !is_safe_single_import_line(line_text) {
            // Bail rather than emit an unsafe edit. The diagnostic still
            // surfaces in the editor; the user fixes by hand for now.
            continue;
        }
        let line_range = whole_line_range(text, line);
        let edit = TextEdit {
            range: line_range,
            new_text: String::new(),
        };
        let mut changes = std::collections::HashMap::new();
        changes.insert(uri.clone(), vec![edit]);
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: "Remove unused import".to_string(),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diag.clone()]),
            edit: Some(WorkspaceEdit {
                changes: Some(changes),
                document_changes: None,
                change_annotations: None,
            }),
            command: None,
            is_preferred: Some(true),
            disabled: None,
            data: None,
        }));
    }
    actions
}

/// Extract the text content of `text`'s line `line` (0-indexed),
/// without the trailing newline. Returns `""` if the line is out of
/// range.
fn nth_line_content(text: &str, line: u32) -> &str {
    let mut current: u32 = 0;
    let mut start: usize = 0;
    let bytes = text.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if current == line {
            // Find end of this line.
            let end = bytes[i..]
                .iter()
                .position(|c| *c == b'\n')
                .map(|p| i + p)
                .unwrap_or(bytes.len());
            return &text[start..end];
        }
        if *b == b'\n' {
            current += 1;
            start = i + 1;
        }
    }
    if current == line {
        return &text[start..];
    }
    ""
}

/// True when `line` is exactly one simple import statement: `import X`,
/// `import X as Y`, or `from M import N` / `from M import N as A` with
/// no commas (which would indicate multiple imports), no semicolons
/// (which would chain a second statement), and no inline `#` comment
/// containing meaningful directives. The leading and trailing
/// whitespace is allowed; a trailing `#` line-comment is also tolerated
/// because dropping it with the import is harmless.
fn is_safe_single_import_line(line: &str) -> bool {
    // Strip a trailing line-comment first; we ignore it for the safety
    // check because it's documentation for the import that's about to
    // disappear anyway. Be careful not to split inside a string literal —
    // imports never contain string literals so a simple `#` search
    // suffices.
    let core = match line.split_once('#') {
        Some((before, _)) => before,
        None => line,
    };
    let trimmed = core.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.contains(';') {
        return false;
    }
    // `import a, b` and `from m import a, b` both signal multiple imports
    // and make the whole-line edit unsafe — only the unused name should
    // be removed in those cases, which the v1 quick-fix doesn't support.
    if trimmed.contains(',') {
        return false;
    }
    trimmed.starts_with("import ") || trimmed.starts_with("from ")
}

/// Convert a resolved binding to an LSP completion item.  The item kind
/// hints the editor's icon (function vs class vs variable); detail is a
/// short type-ish string the editor shows alongside the name.
fn binding_to_completion(binding: &tyc_resolve::Binding) -> CompletionItem {
    let kind = match binding.kind {
        BindingKind::Function => CompletionItemKind::FUNCTION,
        BindingKind::Class => CompletionItemKind::CLASS,
        BindingKind::Parameter => CompletionItemKind::VARIABLE,
        BindingKind::Import => CompletionItemKind::MODULE,
        BindingKind::Loop => CompletionItemKind::VARIABLE,
        BindingKind::Value => match binding.mutability {
            Mutability::Let => CompletionItemKind::CONSTANT,
            Mutability::Mut => CompletionItemKind::VARIABLE,
        },
    };
    let detail = match binding.kind {
        BindingKind::Function => Some("function".to_owned()),
        BindingKind::Class => Some("class".to_owned()),
        BindingKind::Parameter => Some("parameter".to_owned()),
        BindingKind::Import => Some("import".to_owned()),
        BindingKind::Loop => Some("loop binding".to_owned()),
        BindingKind::Value => Some(
            match binding.mutability {
                Mutability::Let => "let",
                Mutability::Mut => "mut",
            }
            .to_owned(),
        ),
    };
    CompletionItem {
        label: binding.name.clone(),
        kind: Some(kind),
        detail,
        ..Default::default()
    }
}

fn simple_completion(
    label: &str,
    kind: CompletionItemKind,
    detail: Option<String>,
) -> CompletionItem {
    CompletionItem {
        label: label.to_owned(),
        kind: Some(kind),
        detail,
        ..Default::default()
    }
}

/// Return the LSP `Range` covering the entirety of `line` (including its
/// terminating newline, so deleting the edit removes the whole line).
fn whole_line_range(text: &str, line: u32) -> Range {
    // Walk to the start of `line` and to the start of `line + 1` (or EOF).
    let mut byte = 0usize;
    let mut seen_lines: u32 = 0;
    let bytes = text.as_bytes();
    let mut start = None;
    while byte < bytes.len() {
        if seen_lines == line && start.is_none() {
            start = Some(byte);
        }
        if seen_lines == line + 1 {
            break;
        }
        if bytes[byte] == b'\n' {
            seen_lines += 1;
        }
        byte += 1;
    }
    if start.is_none() {
        // Line out of range — collapse to a zero-width edit at EOF so the
        // editor can still apply the action without erroring.
        return Range {
            start: byte_to_position(text, bytes.len()),
            end: byte_to_position(text, bytes.len()),
        };
    }
    Range {
        start: byte_to_position(text, start.unwrap()),
        end: byte_to_position(text, byte),
    }
}

/// True when the diagnostic carries a code matching `wanted` (e.g.
/// `tyc::unused_import`).  Used to gate code-action eligibility.
fn diagnostic_code_matches(diag: &Diagnostic, wanted: &str) -> bool {
    match &diag.code {
        Some(NumberOrString::String(s)) => s == wanted,
        _ => false,
    }
}

/// Render the human-friendly explanation for a failed venv
/// introspection so the hover popover surfaces actionable next steps
/// instead of silently omitting the body. Common cases:
///
/// - `NoPython` — no `.venv/bin/python` and no `python3` on `PATH`.
///   Suggest running `tyc sync` (creates the venv) or installing
///   Python.
/// - `ImportFailed` — the chosen interpreter ran but couldn't import
///   the module. Almost always means the package isn't installed in
///   *that* interpreter, even though it may be installed elsewhere.
/// - `Timeout` — exceeded the 10-second cap. Rare; usually points at
///   a misbehaving import-time side effect.
/// - `SpawnFailed` — `python` couldn't be launched at all.
///
/// Returns `None` when there's nothing actionable to surface (no
/// failure recorded — meaning introspection just hasn't been
/// attempted yet for this name).
fn render_introspection_failure(
    failure: &Option<venv_introspect::IntrospectionFailure>,
    python_bin: Option<&std::path::Path>,
    module: &str,
) -> Option<String> {
    let failure = failure.as_ref()?;
    let interp_hint = |p: &std::path::Path| format!("`{}`", p.display());
    let body = match failure {
        venv_introspect::IntrospectionFailure::NoPython => {
            "⚠️ no Python interpreter found — autocomplete is disabled. \
             Create a venv with `tyc sync` or install Python and ensure \
             `python3` is on your `PATH`."
                .to_owned()
        }
        venv_introspect::IntrospectionFailure::ImportFailed { python_bin } => format!(
            "⚠️ `{module}` is not importable in {bin}. \
             Install it with `tyc add {root}` (or `uv pip install {root}` \
             from your project root).",
            module = module,
            bin = interp_hint(python_bin),
            root = module.split('.').next().unwrap_or(module),
        ),
        venv_introspect::IntrospectionFailure::Timeout { python_bin } => format!(
            "⚠️ importing `{module}` in {bin} timed out after 10s — \
             the package may have an expensive import-time side effect. \
             Try `python -c 'import {module}'` directly to reproduce.",
            module = module,
            bin = interp_hint(python_bin),
        ),
        venv_introspect::IntrospectionFailure::SpawnFailed { python_bin } => format!(
            "⚠️ could not launch {bin} to introspect `{module}` — \
             check that the interpreter is executable.",
            bin = interp_hint(python_bin),
            module = module,
        ),
    };
    // `python_bin` arg is passed through but the per-variant data already
    // carries the path on the variants that need it; the outer arg is
    // available for future extensions (e.g. listing which interpreter
    // the cache fell back to).
    let _ = python_bin;
    Some(body)
}

/// Render the hover body for a resolved symbol.  Uses GitHub-flavoured
/// markdown so editors that interpret it (VS Code, JetBrains) format the
/// kind/mutability tags consistently.
fn render_hover(symbol: &SymbolAtOffset<'_>) -> String {
    let Some(def) = symbol.definition else {
        return format!("**{}** — unresolved reference", symbol.name);
    };
    let kind = match def.kind {
        BindingKind::Value => match def.mutability {
            Mutability::Let => "immutable binding",
            Mutability::Mut => "mutable binding",
        },
        BindingKind::Function => "function",
        BindingKind::Class => match def.class_kind {
            ClassKind::Plain => "class",
            ClassKind::Raw => "raw class (`class!`)",
        },
        BindingKind::Parameter => "parameter",
        BindingKind::Import => "import",
        BindingKind::Loop => "loop binding",
    };
    let suffix = if symbol.is_definition {
        " (declaration site)"
    } else {
        ""
    };
    format!("**{}** — *{}*{}", def.name, kind, suffix)
}

/// Render a Python docstring for inclusion in an LSP hover. Strips
/// the common leading-whitespace indent (PEP 257), trims surrounding
/// blank lines, and caps the result so a 500-line module-level
/// docstring doesn't blow up the hover popover. Markdown-renders
/// naturally because the LSP client treats the value as Markdown.
fn render_docstring(doc: &str) -> String {
    // Compute the minimum leading-whitespace indent across non-blank
    // lines (skipping the first line, which is conventionally on the
    // opening triple-quote and has zero leading indent regardless).
    let common_indent: usize = doc
        .lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.chars().take_while(|c| *c == ' ' || *c == '\t').count())
        .min()
        .unwrap_or(0);
    let stripped: Vec<String> = doc
        .lines()
        .enumerate()
        .map(|(i, line)| {
            if i == 0 {
                line.trim_end().to_owned()
            } else if line.len() >= common_indent {
                line[common_indent..].trim_end().to_owned()
            } else {
                line.trim_end().to_owned()
            }
        })
        .collect();
    let trimmed: Vec<&str> = {
        let first = stripped.iter().position(|line| !line.is_empty());
        let last = stripped.iter().rposition(|line| !line.is_empty());
        // When there's no non-blank line, return the empty slice
        // (rather than the full all-blank one) — caller treats the
        // docstring as effectively absent.
        match (first, last) {
            (Some(start), Some(end)) => stripped[start..=end].iter().map(|s| s.as_str()).collect(),
            _ => Vec::new(),
        }
    };
    let formatted = format_docstring_sections(&trimmed);
    let formatted = clean_rst_inline(&formatted);
    // Cap the docstring so a multi-thousand-line module-level docstring
    // (numpy.array has one of these) doesn't flood the popover. 40
    // lines + a `…` continuation marker is enough for the use-case
    // (a quick "what does this do" preview).
    const MAX_LINES: usize = 40;
    let line_count = formatted.lines().count();
    if line_count <= MAX_LINES {
        formatted
    } else {
        let mut out: String = formatted
            .lines()
            .take(MAX_LINES)
            .collect::<Vec<_>>()
            .join("\n");
        out.push_str("\n\n*…(docstring truncated; run `help()` for full text)*");
        out
    }
}

/// Best-effort pass that converts the most common RST inline markup
/// to Markdown so the hover popover doesn't show raw reST. Targets:
///
/// - ``\`\`code\`\``` (RST double-backtick code) → ``\`code\``` (Markdown).
/// - `:class:\`Foo\``, `:func:\`bar\``, `:meth:\`baz\``, `:mod:\`mod\``,
///   `:obj:\`x\``, `:ref:\`label\``, `:exc:\`E\``, `:attr:\`a\``,
///   `:data:\`d\`` — Sphinx cross-reference roles. The role name is
///   stripped, leaving just the referenced symbol in backticks.
///
/// Deliberately conservative: anything that doesn't match exactly
/// passes through unchanged so we don't mangle plain prose that
/// happens to contain a colon or a backtick.
fn clean_rst_inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // ``\`\`code\`\``` → ``\`code\```
        //
        // But NOT triple-backtick Markdown code fences (``` ```python
        // … ``` ```): if we see three or more consecutive backticks,
        // skip past the whole fenced run untouched. Greedily eating
        // a `` `` `` pair out of a fence would collapse the fence
        // marker into malformed inline code and destroy the block.
        if bytes.get(i) == Some(&b'`') && bytes.get(i + 1) == Some(&b'`') {
            if bytes.get(i + 2) == Some(&b'`') {
                // Triple-backtick fence — copy the entire run of
                // consecutive backticks verbatim and continue.
                let run_end = bytes[i..]
                    .iter()
                    .position(|&b| b != b'`')
                    .map(|n| i + n)
                    .unwrap_or(bytes.len());
                out.push_str(&text[i..run_end]);
                i = run_end;
                continue;
            }
            if let Some(rel) = text[i + 2..].find("``") {
                let inner = &text[i + 2..i + 2 + rel];
                out.push('`');
                out.push_str(inner);
                out.push('`');
                i += 2 + rel + 2;
                continue;
            }
        }
        // `:role:\`target\``
        if bytes.get(i) == Some(&b':') {
            // Tolerant scan for `[a-z]+:` followed by a backticked
            // target. Anything else passes through.
            let role_end = text[i + 1..]
                .find(':')
                .map(|n| i + 1 + n)
                .filter(|&n| n - i - 1 > 0 && n - i - 1 < 16);
            if let Some(end) = role_end {
                let role = &text[i + 1..end];
                let after = end + 1;
                if role.chars().all(|c| c.is_ascii_alphabetic() || c == '_')
                    && bytes.get(after) == Some(&b'`')
                {
                    if let Some(rel) = text[after + 1..].find('`') {
                        let target = &text[after + 1..after + 1 + rel];
                        out.push('`');
                        out.push_str(target);
                        out.push('`');
                        i = after + 1 + rel + 1;
                        continue;
                    }
                }
            }
        }
        // Default: copy one UTF-8 character verbatim.
        let ch_len = text[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        out.push_str(&text[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// Detect and re-render structured sections of a Python docstring
/// (Google / NumPy / Sphinx flavour) as proper Markdown so the LSP
/// hover popover shows headings and parameter lists instead of a
/// wall of indented text.
///
/// Supported shapes:
///
/// - **Google.** `Args:` / `Arguments:` / `Returns:` / `Raises:` /
///   `Yields:` / `Examples:` / `Attributes:` / `Note:` / `Warning:`
///   followed by an indented block. Param lines (`name: desc` or
///   `name (type): desc`) become `- **name** — desc` bullets.
/// - **NumPy.** A header line followed by a row of `---` underlines.
///   Parameters use `name : type\n    desc`; we collapse to a
///   single bullet per name.
/// - **Sphinx / reST.** `:param NAME: desc` and `:returns: desc` /
///   `:raises Exc: desc` become bullets under synthesised headings.
///
/// Unknown / unrecognised content is preserved verbatim — falling
/// back to the upstream library's wording is always safer than
/// guessing structure.
fn format_docstring_sections(lines: &[&str]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();
        // NumPy section: `Header\n------` shape (the underline must
        // be at least 3 dashes long). When the header maps to a
        // recognised section name (`Parameters`, `Returns`, …) the
        // canonical form is used so themes can target it. Unknown
        // headers fall back to the *original* text — better than
        // collapsing every section the author invented into a
        // single `**Section**` bucket.
        if i + 1 < lines.len() && is_numpy_underline(lines[i + 1]) && !trimmed.is_empty() {
            let title = canonical_section_title_opt(trimmed)
                .map(str::to_owned)
                .unwrap_or_else(|| trimmed.to_owned());
            out.push(format!("**{title}**"));
            out.push(String::new());
            i += 2;
            // Collect the body lines for this section until the next
            // header (NumPy underline detection) or end of doc.
            let body_start = i;
            while i < lines.len()
                && !(i + 1 < lines.len()
                    && is_numpy_underline(lines[i + 1])
                    && !lines[i].trim().is_empty())
            {
                i += 1;
            }
            render_param_block(&lines[body_start..i], &mut out);
            continue;
        }
        // Google-style header: a section name followed by `:` on a
        // line by itself.
        if let Some(title) = google_section_header(line) {
            out.push(format!("**{title}**"));
            out.push(String::new());
            i += 1;
            // The Google block is bounded by the next non-indented
            // line. Headers themselves are flush-left after the
            // PEP 257 strip; the block is one indent level deeper.
            let body_start = i;
            while i < lines.len()
                && (lines[i].starts_with(' ') || lines[i].starts_with('\t') || lines[i].is_empty())
            {
                i += 1;
            }
            render_param_block(&lines[body_start..i], &mut out);
            continue;
        }
        // Sphinx / reST `:param X: desc`. We collect runs into a
        // synthetic Parameters heading so the user gets a list.
        // Continuation lines (indented body under a `:param`) are
        // appended to the current bullet so multi-line parameter
        // descriptions survive the re-render — dropping them would
        // silently lose docstring content.
        if trimmed.starts_with(":param ") || trimmed.starts_with(":parameter ") {
            let block_start = i;
            while i < lines.len()
                && (lines[i].trim_start().starts_with(":param ")
                    || lines[i].trim_start().starts_with(":parameter ")
                    || (i > block_start && (lines[i].starts_with(' ') || lines[i].is_empty())))
            {
                i += 1;
            }
            out.push("**Parameters**".to_owned());
            out.push(String::new());
            let mut current_bullet: Option<String> = None;
            for sphinx_line in &lines[block_start..i] {
                let stripped = sphinx_line.trim_start();
                if let Some(bullet) = sphinx_param_to_bullet(stripped) {
                    if let Some(prev) = current_bullet.take() {
                        out.push(prev);
                    }
                    current_bullet = Some(bullet);
                } else if !stripped.is_empty() {
                    if let Some(b) = current_bullet.as_mut() {
                        b.push(' ');
                        b.push_str(stripped);
                    }
                }
            }
            if let Some(b) = current_bullet.take() {
                out.push(b);
            }
            continue;
        }
        // Sphinx `:raises Exc: desc` (and the singular `:raise`).
        // Collected like `:param` so a run of consecutive raises
        // directives produces a single **Raises** section.
        if trimmed.starts_with(":raises ")
            || trimmed.starts_with(":raise ")
            || trimmed.starts_with(":except ")
        {
            let block_start = i;
            while i < lines.len()
                && (lines[i].trim_start().starts_with(":raises ")
                    || lines[i].trim_start().starts_with(":raise ")
                    || lines[i].trim_start().starts_with(":except ")
                    || (i > block_start && (lines[i].starts_with(' ') || lines[i].is_empty())))
            {
                i += 1;
            }
            out.push("**Raises**".to_owned());
            out.push(String::new());
            let mut current_bullet: Option<String> = None;
            for sphinx_line in &lines[block_start..i] {
                let stripped = sphinx_line.trim_start();
                if let Some(bullet) = sphinx_raises_to_bullet(stripped) {
                    if let Some(prev) = current_bullet.take() {
                        out.push(prev);
                    }
                    current_bullet = Some(bullet);
                } else if !stripped.is_empty() {
                    if let Some(b) = current_bullet.as_mut() {
                        b.push(' ');
                        b.push_str(stripped);
                    }
                }
            }
            if let Some(b) = current_bullet.take() {
                out.push(b);
            }
            continue;
        }
        if trimmed.starts_with(":returns:") || trimmed.starts_with(":return:") {
            let desc = trimmed
                .trim_start_matches(":returns:")
                .trim_start_matches(":return:")
                .trim();
            out.push("**Returns**".to_owned());
            out.push(String::new());
            if !desc.is_empty() {
                out.push(desc.to_owned());
            }
            i += 1;
            continue;
        }
        // Fallback — preserve the line verbatim.
        out.push(line.to_owned());
        i += 1;
    }
    // Collapse runs of more than one blank line — the re-rendering
    // can leave empty entries where source had a single blank.
    let mut collapsed: Vec<String> = Vec::with_capacity(out.len());
    let mut prev_blank = false;
    for line in out {
        let is_blank = line.is_empty();
        if is_blank && prev_blank {
            continue;
        }
        prev_blank = is_blank;
        collapsed.push(line);
    }
    collapsed.join("\n")
}

/// `Args:` → `Args`. `args:` and `Args :` are also accepted (tolerant
/// to author typos). Returns `None` when the line isn't a Google-style
/// section header.
fn google_section_header(line: &str) -> Option<&'static str> {
    let trimmed = line.trim();
    let body = trimmed.strip_suffix(':')?;
    canonical_section_title_opt(body.trim())
}

/// NumPy underline: a line of only `-` / `=` / `~` characters, at
/// least 3 long. NumPy itself uses `-`; some authors use `=`.
fn is_numpy_underline(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= 3 && trimmed.chars().all(|c| c == '-' || c == '=' || c == '~')
}

/// Map a section name to its canonical capitalised form. Returns
/// `None` for unrecognised names.
fn canonical_section_title_opt(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_lowercase().as_str() {
        "args" | "arguments" | "params" | "parameters" => "Parameters",
        "returns" | "return" => "Returns",
        "yields" | "yield" => "Yields",
        "raises" | "raise" | "except" | "exceptions" => "Raises",
        "examples" | "example" => "Examples",
        "attributes" => "Attributes",
        "note" | "notes" => "Note",
        "warning" | "warnings" => "Warning",
        "see also" => "See Also",
        _ => return None,
    })
}

/// Render the contents of a parameter block (Google `Args:` body or
/// NumPy `Parameters` body) as Markdown bullets. Lines that don't
/// look like `name: description` are preserved verbatim so prose
/// inside a section doesn't get mangled.
fn render_param_block(body: &[&str], out: &mut Vec<String>) {
    // Detect the indent the block's first param line uses; subsequent
    // continuation lines that share or exceed that indent are folded
    // into the same bullet.
    let mut current_bullet: Option<String> = None;
    for line in body {
        if line.trim().is_empty() {
            if let Some(b) = current_bullet.take() {
                out.push(b);
            }
            continue;
        }
        let stripped = line.trim_start();
        // Detect `name: description` (Google) or `name : type` (NumPy).
        if let Some((name, rest)) = split_param_line(stripped) {
            if let Some(b) = current_bullet.take() {
                out.push(b);
            }
            let bullet = if rest.is_empty() {
                format!("- **{name}**")
            } else {
                format!("- **{name}** — {rest}")
            };
            current_bullet = Some(bullet);
        } else if current_bullet.is_some() {
            // Continuation of the previous bullet.
            if let Some(b) = current_bullet.as_mut() {
                b.push(' ');
                b.push_str(stripped);
            }
        } else {
            // Free prose inside the section.
            if let Some(b) = current_bullet.take() {
                out.push(b);
            }
            out.push(line.to_string());
        }
    }
    if let Some(b) = current_bullet.take() {
        out.push(b);
    }
}

/// Split `name: description` (Google) or `name (type): description`
/// or `name : type` (NumPy first line) into `(name, description)`.
/// Returns `None` when the line doesn't fit any of those shapes.
fn split_param_line(line: &str) -> Option<(String, String)> {
    let (head, tail) = line.split_once(':')?;
    let head = head.trim();
    // Names must look like Python identifiers (possibly with a
    // trailing `(type)` annotation, which we strip).
    let name = head.split_whitespace().next()?.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some((name.to_owned(), tail.trim().to_owned()))
}

/// `:param name: description` → `- **name** — description`.
fn sphinx_param_to_bullet(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix(":param ")
        .or_else(|| line.strip_prefix(":parameter "))?;
    let (name, desc) = rest.split_once(':')?;
    Some(format!("- **{}** — {}", name.trim(), desc.trim()))
}

/// `:raises ValueError: description` → `- **ValueError** — description`.
/// Accepts the singular `:raise` and the obscure `:except` variants
/// the older docutils dialect used.
fn sphinx_raises_to_bullet(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix(":raises ")
        .or_else(|| line.strip_prefix(":raise "))
        .or_else(|| line.strip_prefix(":except "))?;
    let (exc, desc) = rest.split_once(':')?;
    Some(format!("- **{}** — {}", exc.trim(), desc.trim()))
}

/// `inspect.signature(obj)` returns the pretty form `(arg1, arg2=…)`,
/// which the Python helper script prepends with `name`. When the name
/// has been stitched on for completion / hover purposes but we want
/// the parenthesised tail on its own (so the code-block reads as a
/// proper Python signature), peel off the leading identifier.
///
/// Defensive: if the signature doesn't start with the target name,
/// return it unchanged — the script may have returned an unfamiliar
/// shape for C extensions or stub-less builtins.
fn sig_tail<'a>(sig: &'a str, name: &str) -> &'a str {
    sig.strip_prefix(name).unwrap_or(sig)
}

/// Convert an LSP `Position` (line + UTF-16 column) to a byte offset.
/// Out-of-range positions clamp to the end of the document.
fn position_to_byte(source: &str, position: Position) -> usize {
    let mut current_line: u32 = 0;
    let mut current_column: u32 = 0;
    let mut byte: usize = 0;
    for ch in source.chars() {
        if current_line == position.line && current_column >= position.character {
            return byte;
        }
        if ch == '\n' {
            if current_line == position.line {
                // Position requested beyond this line's content; clamp here.
                return byte;
            }
            current_line += 1;
            current_column = 0;
        } else if current_line == position.line {
            current_column += ch.len_utf16() as u32;
        }
        byte += ch.len_utf8();
    }
    byte
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Convert a Typhon diagnostic to an LSP `Diagnostic`. Returns `None` when
/// the diagnostic carries no positional label (e.g. an I/O error that is
/// not anchored to a specific source span — those go through the LSP log
/// channel instead, handled by the caller).
fn tyc_error_to_lsp(
    err: &TycError,
    source: &str,
    severity: DiagnosticSeverity,
) -> Option<Diagnostic> {
    let label = first_label(err)?;
    let span = label.inner();
    let start = byte_to_position(source, span.offset());
    let end = byte_to_position(source, span.offset() + span.len());
    let message = err.to_string();
    let code = err.code().map(|c| NumberOrString::String(c.to_string()));
    Some(Diagnostic {
        range: Range { start, end },
        severity: Some(severity),
        code,
        code_description: None,
        source: Some("tyc".into()),
        message,
        related_information: None,
        tags: None,
        data: None,
    })
}

fn first_label(err: &TycError) -> Option<LabeledSpan> {
    err.labels()?.next()
}

/// Pick the text that a diagnostic's byte offsets are anchored to.
///
/// `validate_question_ops` errors carry offsets into the **original** Typhon
/// source. Every other diagnostic produced by `check_file` (parse, resolve,
/// type-check, unused-import, etc.) is anchored to the **preprocessed**
/// source because that's what the parser and resolver see after `let`/`mut`
/// stripping and sugar expansion. Selecting the correct reference text per
/// diagnostic variant keeps published LSP ranges aligned with the editor
/// buffer instead of drifting by a column or two after `val` is removed.
fn diagnostic_source<'a>(err: &TycError, original: &'a str, preprocessed: &'a str) -> &'a str {
    match err {
        // Both validators run against the original Typhon source, before any
        // sugar expansion, so their byte offsets refer to the editor buffer.
        TycError::InvalidQuestionOp { .. } | TycError::LazyUsage { .. } => original,
        _ => preprocessed,
    }
}

/// Convert a byte offset in `source` to an LSP `Position` (line + UTF-16
/// column). Out-of-range offsets clamp to the end of the document.
///
/// LSP defaults to UTF-16 code units for column counts, so we sum
/// `char::len_utf16()` rather than byte length.
fn byte_to_position(source: &str, target: usize) -> Position {
    let mut line: u32 = 0;
    let mut column: u32 = 0;
    let mut byte: usize = 0;
    for ch in source.chars() {
        if byte >= target {
            break;
        }
        if ch == '\n' {
            line += 1;
            column = 0;
        } else {
            column += ch.len_utf16() as u32;
        }
        byte += ch.len_utf8();
    }
    Position {
        line,
        character: column,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_to_position_ascii() {
        let src = "abc\ndef\nghij";
        // Beginning.
        assert_eq!(
            byte_to_position(src, 0),
            Position {
                line: 0,
                character: 0
            }
        );
        // Middle of first line.
        assert_eq!(
            byte_to_position(src, 2),
            Position {
                line: 0,
                character: 2
            }
        );
        // Start of second line (after first `\n`).
        assert_eq!(
            byte_to_position(src, 4),
            Position {
                line: 1,
                character: 0
            }
        );
        // Inside third line.
        assert_eq!(
            byte_to_position(src, 9),
            Position {
                line: 2,
                character: 1
            }
        );
    }

    #[test]
    fn byte_to_position_handles_multibyte() {
        // 'é' is 2 bytes in UTF-8 but 1 code unit in UTF-16.
        let src = "café\n";
        // Position of '\n' (byte 5) is column 4 (one UTF-16 unit per char).
        assert_eq!(
            byte_to_position(src, 5),
            Position {
                line: 0,
                character: 4
            }
        );
    }

    #[test]
    fn log_level_parses_common_spellings() {
        assert!(matches!(LogLevel::parse("error"), LogLevel::Error));
        assert!(matches!(LogLevel::parse("ERROR"), LogLevel::Error));
        assert!(matches!(LogLevel::parse("warn"), LogLevel::Warn));
        assert!(matches!(LogLevel::parse("warning"), LogLevel::Warn));
        assert!(matches!(LogLevel::parse("info"), LogLevel::Info));
        assert!(matches!(LogLevel::parse("debug"), LogLevel::Debug));
        assert!(matches!(LogLevel::parse("trace"), LogLevel::Debug));
        // Unknown values fall back to Info rather than dropping all logs,
        // so a typo doesn't silently mute the channel.
        assert!(matches!(LogLevel::parse("bogus"), LogLevel::Info));
    }

    #[test]
    fn byte_to_position_clamps_to_end() {
        let src = "x = 1";
        assert_eq!(
            byte_to_position(src, 100),
            Position {
                line: 0,
                character: 5
            }
        );
    }

    #[test]
    fn position_to_byte_round_trips_ascii() {
        let src = "abc\ndef\nghij";
        // Round-trip the same set of offsets used by byte_to_position_ascii.
        for offset in [0usize, 2, 4, 9] {
            let pos = byte_to_position(src, offset);
            assert_eq!(position_to_byte(src, pos), offset);
        }
    }

    #[test]
    fn position_to_byte_handles_multibyte() {
        // `café\n` occupies bytes 0..6 ('é' is 2 bytes), `x` starts at byte 6.
        // Column 0 on line 1 therefore maps to byte 6.
        let src = "café\nx";
        let pos = Position {
            line: 1,
            character: 0,
        };
        assert_eq!(position_to_byte(src, pos), 6);
    }

    #[test]
    fn render_docstring_strips_pep257_indent() {
        // Python docstring with the typical 4-space indent on
        // continuation lines. Renderer must strip the common indent
        // before delegating to the section formatter. Without the
        // PEP 257 pass, every body line would still be indented and
        // the `Args:` header detection would miss the section.
        let doc = "Build a new Agent.\n\n    Args:\n        name: human-readable label.\n        client: the LLM client to call.\n    ";
        let out = render_docstring(doc);
        // First line preserved as the docstring intro.
        assert!(out.starts_with("Build a new Agent."), "intro: {out}");
        // PEP 257 indent stripped: the section formatter would only
        // match `Args:` flush-left, which it does in this output.
        assert!(out.contains("**Parameters**"), "section header: {out}");
        assert!(
            out.contains("- **name** — human-readable label."),
            "name bullet: {out}"
        );
    }

    #[test]
    fn render_docstring_truncates_at_40_lines() {
        // A pathological multi-thousand-line docstring (numpy.array
        // ships one of these) must not flood the hover popover.
        let mut doc = String::new();
        for i in 0..200 {
            doc.push_str(&format!("line {i}\n"));
        }
        let out = render_docstring(&doc);
        assert!(out.contains("line 0"), "first line preserved");
        assert!(out.contains("line 39"), "last kept line preserved");
        assert!(!out.contains("line 40"), "first dropped line absent");
        assert!(
            out.contains("docstring truncated"),
            "truncation marker present: {out}"
        );
    }

    #[test]
    fn render_docstring_handles_first_line_only() {
        let out = render_docstring("Add two numbers.");
        assert_eq!(out, "Add two numbers.");
    }

    #[test]
    fn render_docstring_formats_google_style_args() {
        // Google-style `Args:` block — the most common shape in
        // modern Python. The renderer must produce a bold section
        // header and bullet each parameter so the hover popover
        // reads as structured text.
        let doc = "Build a new Agent.\n\n    Args:\n        name: human-readable label.\n        client: the LLM client to call.\n        tools: list of tool callables.\n\n    Returns:\n        A configured Agent instance.\n";
        let out = render_docstring(doc);
        assert!(out.contains("**Parameters**"), "Args header: {out}");
        assert!(
            out.contains("- **name** — human-readable label."),
            "name bullet: {out}"
        );
        assert!(
            out.contains("- **client** — the LLM client to call."),
            "client bullet: {out}"
        );
        assert!(out.contains("**Returns**"), "Returns header: {out}");
    }

    #[test]
    fn render_docstring_formats_numpy_style_parameters() {
        // NumPy's `Parameters\n----------` underline shape.
        let doc = "Compute the dot product.\n\nParameters\n----------\na : ndarray\n    First operand.\nb : ndarray\n    Second operand.\n\nReturns\n-------\nfloat\n    The scalar dot product.\n";
        let out = render_docstring(doc);
        assert!(out.contains("**Parameters**"));
        assert!(out.contains("- **a**"), "a bullet: {out}");
        assert!(out.contains("- **b**"), "b bullet: {out}");
        assert!(out.contains("**Returns**"));
    }

    #[test]
    fn render_docstring_formats_sphinx_param_directives() {
        // Sphinx / reST `:param name: desc` directives.
        let doc = "Connect to the server.\n\n:param host: hostname to dial.\n:param port: TCP port.\n:returns: a Connection.\n";
        let out = render_docstring(doc);
        assert!(out.contains("**Parameters**"));
        assert!(out.contains("- **host** — hostname to dial."));
        assert!(out.contains("- **port** — TCP port."));
        assert!(out.contains("**Returns**"));
        assert!(out.contains("a Connection."));
    }

    #[test]
    fn render_docstring_preserves_unrecognised_sections() {
        // Free prose without any section headers passes through
        // unchanged — better to be conservative than to mangle.
        let doc = "Simple one-liner with no structure.";
        let out = render_docstring(doc);
        assert_eq!(out, "Simple one-liner with no structure.");
    }

    #[test]
    fn render_docstring_formats_sphinx_raises_directive() {
        // `:raises Exc: desc` was advertised in the PR but the
        // original implementation had no handler — the directive
        // would have fallen through verbatim. Regression: collect
        // a run of `:raises` lines under a synthesised **Raises**
        // heading with bullets.
        let doc = "Connect.\n\n:raises ConnectionError: dial failed.\n:raises TimeoutError: server didn't respond.\n";
        let out = render_docstring(doc);
        assert!(out.contains("**Raises**"), "header: {out}");
        assert!(
            out.contains("- **ConnectionError** — dial failed."),
            "first bullet: {out}"
        );
        assert!(
            out.contains("- **TimeoutError** — server didn't respond."),
            "second bullet: {out}"
        );
    }

    #[test]
    fn render_docstring_folds_sphinx_param_continuations() {
        // Multi-line `:param` descriptions used to get dropped:
        // the block collector grabbed the continuation lines but
        // the renderer only emitted bullets for `:param` directives.
        // Continuation text now folds into the prior bullet so no
        // docstring content is lost.
        let doc = "Open.\n\n:param host: hostname to dial,\n    accepts IPv4 and IPv6 addresses.\n:param port: TCP port.\n";
        let out = render_docstring(doc);
        let host_bullet = out
            .lines()
            .find(|l| l.starts_with("- **host**"))
            .expect("host bullet present");
        assert!(
            host_bullet.contains("accepts IPv4 and IPv6 addresses."),
            "continuation folded in: {host_bullet}"
        );
    }

    #[test]
    fn render_docstring_preserves_unknown_numpy_header_text() {
        // NumPy underlines can sit under any header the author
        // wrote. When the title doesn't match one of the curated
        // sections (`Parameters`, `Returns`, …) the original text
        // is preserved instead of being collapsed to a generic
        // **Section** stub.
        let doc = "Compute.\n\nGotchas\n-------\nWatch out for division by zero.\n";
        let out = render_docstring(doc);
        assert!(
            out.contains("**Gotchas**"),
            "original header preserved: {out}"
        );
        assert!(!out.contains("**Section**"), "no Section stub: {out}");
        assert!(out.contains("Watch out for division by zero."));
    }

    #[test]
    fn render_docstring_preserves_markdown_code_fences() {
        // ` ```python … ``` ` is a Markdown code fence and must
        // pass through the RST inline cleanup intact. The earlier
        // pass searched for the next `` `` `` globally, which would
        // eat the fence opener and collapse the block into
        // malformed inline backticks.
        let doc = "Build it.\n\n```python\nfoo()\n```\n";
        let out = render_docstring(doc);
        assert!(out.contains("```python"), "fence opener preserved: {out}");
        assert!(
            out.trim_end().ends_with("```"),
            "fence closer preserved: {out:?}"
        );
        assert!(out.contains("foo()"), "fenced body preserved: {out}");
    }

    #[test]
    fn render_docstring_cleans_rst_double_backtick_code() {
        // RST uses ``\`\`code\`\``` for inline code; Markdown wants
        // a single backtick. The hover popover was showing the
        // double backticks literally instead of rendering as code.
        let doc = "Returns ``True`` when the input is non-empty.";
        let out = render_docstring(doc);
        assert!(!out.contains("``True``"), "doubles stripped: {out}");
        assert!(out.contains("`True`"), "single backticks left: {out}");
    }

    #[test]
    fn render_docstring_cleans_sphinx_role_directives() {
        // `:class:\`Foo\`` is a Sphinx cross-reference. The role name
        // should drop, leaving just the backticked symbol so the
        // popover doesn't surface raw reST role syntax.
        let doc = "Construct a :class:`Agent` configured with a :func:`make_client` factory.";
        let out = render_docstring(doc);
        assert!(out.contains("`Agent`"), "class ref kept: {out}");
        assert!(out.contains("`make_client`"), "func ref kept: {out}");
        assert!(!out.contains(":class:"), "role stripped: {out}");
        assert!(!out.contains(":func:"), "role stripped: {out}");
    }

    #[test]
    fn render_docstring_handles_examples_section() {
        let doc = "Do the thing.\n\n    Examples:\n        >>> do_the_thing()\n        42\n";
        let out = render_docstring(doc);
        assert!(out.contains("**Examples**"), "header: {out}");
        assert!(out.contains(">>> do_the_thing()"), "code line: {out}");
    }

    #[test]
    fn render_docstring_empty_input_is_empty() {
        // The Python script returns `None` for empty docstrings, but
        // the helper has to be defensive against an explicit empty
        // string sneaking through (e.g. `__doc__ = ""`).
        assert_eq!(render_docstring(""), "");
        assert_eq!(render_docstring("   \n  \n  "), "");
    }

    #[test]
    fn sig_tail_strips_leading_name() {
        // `inspect.signature(Cls)` returns `(arg1, arg2=…)`; the
        // Python script prefixes the class name so completions read
        // as a full signature. For the code-block in hover we want
        // the parenthesised tail alone.
        assert_eq!(
            sig_tail("Agent(*, name, client)", "Agent"),
            "(*, name, client)"
        );
    }

    #[test]
    fn sig_tail_passes_through_unknown_shape() {
        // C extensions with unusual signature pretty-printing
        // shouldn't be mangled by the prefix strip.
        assert_eq!(sig_tail("[built-in]", "Agent"), "[built-in]");
    }

    #[test]
    fn render_hover_describes_val_binding() {
        use tyc_resolve::{Binding, BindingKind, ClassKind, Mutability};
        let binding = Binding {
            name: "x".to_owned(),
            kind: BindingKind::Value,
            mutability: Mutability::Let,
            span: (4, 5),
            import_info: None,
            class_kind: ClassKind::Plain,
        };
        let symbol = SymbolAtOffset {
            name: "x".to_owned(),
            span: (4, 5),
            definition: Some(&binding),
            is_definition: true,
        };
        let body = render_hover(&symbol);
        assert!(body.contains("immutable"), "got: {body}");
        assert!(body.contains("declaration site"), "got: {body}");
    }

    #[test]
    fn render_hover_describes_function() {
        use tyc_resolve::{Binding, BindingKind, ClassKind, Mutability};
        let binding = Binding {
            name: "main".to_owned(),
            kind: BindingKind::Function,
            mutability: Mutability::Mut,
            span: (4, 8),
            import_info: None,
            class_kind: ClassKind::Plain,
        };
        let symbol = SymbolAtOffset {
            name: "main".to_owned(),
            span: (10, 14),
            definition: Some(&binding),
            is_definition: false,
        };
        let body = render_hover(&symbol);
        assert!(body.contains("function"), "got: {body}");
        assert!(!body.contains("declaration site"), "got: {body}");
    }

    #[test]
    fn render_hover_distinguishes_raw_class() {
        use tyc_resolve::{Binding, BindingKind, ClassKind, Mutability};
        let raw = Binding {
            name: "MyModel".to_owned(),
            kind: BindingKind::Class,
            mutability: Mutability::Mut,
            span: (0, 7),
            import_info: None,
            class_kind: ClassKind::Raw,
        };
        let plain = Binding {
            name: "User".to_owned(),
            kind: BindingKind::Class,
            mutability: Mutability::Mut,
            span: (0, 4),
            import_info: None,
            class_kind: ClassKind::Plain,
        };
        let raw_sym = SymbolAtOffset {
            name: raw.name.clone(),
            span: raw.span,
            definition: Some(&raw),
            is_definition: true,
        };
        let plain_sym = SymbolAtOffset {
            name: plain.name.clone(),
            span: plain.span,
            definition: Some(&plain),
            is_definition: true,
        };
        let raw_body = render_hover(&raw_sym);
        let plain_body = render_hover(&plain_sym);
        assert!(raw_body.contains("raw class"), "got: {raw_body}");
        assert!(raw_body.contains("class!"), "got: {raw_body}");
        assert!(!plain_body.contains("raw class"), "got: {plain_body}");
    }

    // ── cross-file import resolution ─────────────────────────────────────

    #[test]
    fn resolve_module_to_file_prefers_direct_module_over_package() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("pkg")).unwrap();
        std::fs::write(src.join("util.ty"), "let x: int = 1\n").unwrap();
        std::fs::write(src.join("pkg").join("__init__.ty"), "let y: int = 2\n").unwrap();

        let direct = resolve_module_to_file(&src, "util").expect("util.ty must resolve");
        assert_eq!(direct.file_name().unwrap(), "util.ty");

        let pkg = resolve_module_to_file(&src, "pkg").expect("pkg/__init__.ty must resolve");
        assert_eq!(pkg.file_name().unwrap(), "__init__.ty");
        assert!(pkg.parent().unwrap().ends_with("pkg"));
    }

    #[test]
    fn project_module_members_surfaces_public_top_level_bindings() {
        // src/utils.ty exposes `helper` (def), `MAX` (let), `_private`
        // (def), and re-exports `path` from `os`. The completion menu
        // for `from utils import <cursor>` should include `helper` and
        // `MAX` only — underscore-prefixed names and import re-exports
        // shouldn't surface.
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("utils.ty"),
            "import os\n\
             from os import path\n\
             \n\
             let MAX: int = 10\n\
             \n\
             def helper() -> None:\n    \
                 pass\n\
             \n\
             def _private() -> None:\n    \
                 pass\n",
        )
        .unwrap();

        let items = project_module_members(&src, "utils").expect("utils.ty must resolve");
        let labels: std::collections::HashSet<String> =
            items.iter().map(|i| i.label.clone()).collect();

        assert!(
            labels.contains("helper"),
            "expected `helper`, got {labels:?}"
        );
        assert!(labels.contains("MAX"), "expected `MAX`, got {labels:?}");
        assert!(
            !labels.contains("_private"),
            "underscore-prefixed names must not surface: {labels:?}"
        );
        assert!(
            !labels.contains("os") && !labels.contains("path"),
            "import bindings must not surface as re-exports: {labels:?}"
        );
    }

    #[test]
    fn project_module_members_resolves_package_init_file() {
        // `from pkg import <cursor>` should resolve through
        // `src/pkg/__init__.ty` — same as Python's package model.
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("pkg")).unwrap();
        std::fs::write(
            src.join("pkg").join("__init__.ty"),
            "def greet() -> None:\n    pass\n",
        )
        .unwrap();

        let items = project_module_members(&src, "pkg").expect("pkg/__init__.ty must resolve");
        assert!(
            items.iter().any(|i| i.label == "greet"),
            "expected `greet` from pkg/__init__.ty"
        );
    }

    #[test]
    fn project_module_members_returns_none_for_missing_module() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        assert!(project_module_members(&src, "nope").is_none());
    }

    #[test]
    fn compute_module_path_maps_files_to_dotted_modules() {
        let src = std::path::Path::new("/proj/src");
        assert_eq!(
            compute_module_path(src, std::path::Path::new("/proj/src/utils.ty")).as_deref(),
            Some("utils")
        );
        assert_eq!(
            compute_module_path(src, std::path::Path::new("/proj/src/tools/web_tools.ty"))
                .as_deref(),
            Some("tools.web_tools")
        );
        assert_eq!(
            compute_module_path(src, std::path::Path::new("/proj/src/pkg/__init__.ty")).as_deref(),
            Some("pkg")
        );
        assert_eq!(
            compute_module_path(src, std::path::Path::new("/proj/src/pkg/sub/__init__.ty"))
                .as_deref(),
            Some("pkg.sub")
        );
    }

    #[test]
    fn project_index_indexes_top_level_publics_across_files() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("tools")).unwrap();
        std::fs::write(src.join("utils.ty"), "def helper() -> None:\n    pass\n").unwrap();
        std::fs::write(
            src.join("tools").join("web_tools.ty"),
            "def web_search() -> None:\n    pass\n\nlet TIMEOUT: int = 30\n",
        )
        .unwrap();

        let mut index = ProjectIndex::default();
        index.refresh(&src);

        // `helper` is exported by `utils`.
        let helper = index.by_name.get("helper").expect("helper indexed");
        assert_eq!(helper.len(), 1);
        assert_eq!(helper[0].module, "utils");
        assert_eq!(helper[0].kind, BindingKind::Function);

        // `web_search` lives in the nested module.
        let web_search = index.by_name.get("web_search").expect("web_search indexed");
        assert_eq!(web_search[0].module, "tools.web_tools");

        // Top-level `let` constants also surface.
        assert!(index.by_name.contains_key("TIMEOUT"));
    }

    #[test]
    fn project_index_drops_stale_entries_on_refresh() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("utils.ty");
        std::fs::write(&file, "def helper() -> None:\n    pass\n").unwrap();

        let mut index = ProjectIndex::default();
        index.refresh(&src);
        assert!(index.by_name.contains_key("helper"));

        std::fs::remove_file(&file).unwrap();
        index.refresh(&src);
        assert!(
            !index.by_name.contains_key("helper"),
            "stale entry should be dropped"
        );
    }

    #[test]
    fn auto_import_text_edit_inserts_after_existing_imports() {
        let src = "import os\nfrom math import pi\n\ndef main() -> None:\n    pass\n";
        let edit = auto_import_text_edit_at(auto_import_insertion_range(src), "utils", "helper");
        // Insert on the line *after* the last import (index 2 = the blank line).
        assert_eq!(edit.range.start.line, 2);
        assert_eq!(edit.range.start.character, 0);
        assert_eq!(edit.new_text, "from utils import helper\n");
    }

    #[test]
    fn auto_import_text_edit_inserts_at_top_when_no_imports() {
        let src = "def main() -> None:\n    pass\n";
        let edit = auto_import_text_edit_at(auto_import_insertion_range(src), "utils", "helper");
        assert_eq!(edit.range.start.line, 0);
        assert_eq!(edit.range.start.character, 0);
    }

    #[test]
    fn auto_import_text_edit_skips_shebang() {
        let src = "#!/usr/bin/env tyc\ndef main() -> None:\n    pass\n";
        let edit = auto_import_text_edit_at(auto_import_insertion_range(src), "utils", "helper");
        // Lands on the line *after* the shebang.
        assert_eq!(edit.range.start.line, 1);
    }

    #[test]
    fn auto_import_text_edit_skips_module_docstring() {
        let src = "\"\"\"Module that does things.\"\"\"\n\ndef main() -> None:\n    pass\n";
        let edit = auto_import_text_edit_at(auto_import_insertion_range(src), "utils", "helper");
        // Inserted on the line right after the (single-line) docstring.
        assert_eq!(edit.range.start.line, 1);
    }

    #[test]
    fn auto_import_text_edit_skips_multiline_docstring() {
        let src =
            "\"\"\"\nLong docstring\nspanning lines.\n\"\"\"\ndef main() -> None:\n    pass\n";
        let edit = auto_import_text_edit_at(auto_import_insertion_range(src), "utils", "helper");
        // The docstring ends on line 3 (0-indexed); insert lands at line 4.
        assert_eq!(edit.range.start.line, 4);
    }

    #[test]
    fn auto_import_text_edit_skips_shebang_then_docstring() {
        let src = "#!/usr/bin/env tyc\n\"\"\"doc\"\"\"\ndef main() -> None:\n    pass\n";
        let edit = auto_import_text_edit_at(auto_import_insertion_range(src), "utils", "helper");
        // Past shebang (line 0) and docstring (line 1) → line 2.
        assert_eq!(edit.range.start.line, 2);
    }

    #[test]
    fn auto_import_text_edit_ignores_nested_imports() {
        // No top-level imports — the indented `import sys` inside the
        // function body must NOT anchor the auto-import (that would
        // drop a top-level statement into the function).
        let src = "def main() -> None:\n    import sys\n    pass\n";
        let edit = auto_import_text_edit_at(auto_import_insertion_range(src), "utils", "helper");
        // Falls through to the no-imports path → line 0.
        assert_eq!(edit.range.start.line, 0);
    }

    #[test]
    fn resolve_module_to_file_returns_none_for_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        assert!(resolve_module_to_file(&src, "missing").is_none());
    }

    #[test]
    fn find_workspace_layout_walks_up_to_typhon_toml() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("typhon.toml"),
            "[project]\nname=\"x\"\nsrc=\"lib\"\n",
        )
        .unwrap();
        let src = tmp.path().join("lib");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("main.ty");
        std::fs::write(&file, "let x: int = 1\n").unwrap();

        let (root, src_dir) = find_workspace_layout(&file).expect("layout should be detected");
        assert_eq!(root, tmp.path());
        assert_eq!(src_dir, src);
    }

    #[test]
    fn parse_src_dir_extracts_quoted_value() {
        let tmp = tempfile::tempdir().unwrap();
        let toml = tmp.path().join("typhon.toml");
        std::fs::write(&toml, "[project]\nsrc = \"src\"\n").unwrap();
        assert_eq!(parse_src_dir(&toml).as_deref(), Some("src"));
    }

    #[test]
    fn percent_round_trip_preserves_spaces() {
        let s = "/tmp/some dir/file.ty";
        let enc = percent_encode(s);
        assert!(enc.contains("%20"), "{enc}");
        assert_eq!(percent_decode(&enc), s);
    }

    // ── resolver integration ─────────────────────────────────────────────

    #[test]
    fn resolver_finds_bindings_in_preprocessed_source() {
        // After preprocessing, `let x` becomes `x: int = 1`.  The resolver
        // should see `x` as a binding at offset 0.
        let preprocessed = "x: int = 1\n";
        let module = tyc_syntax::parse_module(preprocessed)
            .expect("valid source should parse")
            .into_syntax();
        let (resolved, _) = tyc_resolve::resolve_module("<test>".to_owned(), preprocessed, &module);
        let names: Vec<String> = resolved
            .module_scope()
            .bindings
            .iter()
            .map(|b| b.name.clone())
            .collect();
        assert!(
            names.contains(&"x".to_owned()),
            "x should be resolved; got {names:?}"
        );
    }

    #[test]
    fn resolver_parse_error_yields_default_module() {
        // The `resolved_module` Salsa query returns `Arc::new(ResolvedModule::default())`
        // on a parse failure.  Verify that behavior via the db directly.
        let db = TycDatabase::new();
        let file = SourceFile::new(&db, "broken.ty".into(), "def (broken syntax)\n".into());
        let resolved = resolved_module_arc(&db, file);
        // A default ResolvedModule has empty scopes and references — not a panic or None.
        assert!(
            resolved.scopes.is_empty(),
            "default module on parse failure should have no scopes"
        );
        assert!(
            resolved.references.is_empty(),
            "default module on parse failure should have no references"
        );
    }

    // ── Phase 4: completion ──────────────────────────────────────────────

    fn parse_resolved(src: &str) -> (ResolvedModule, String) {
        let prep = tyc_syntax::preprocess::preprocess(src);
        let module = tyc_syntax::parse_module(&prep.python_source)
            .unwrap()
            .into_syntax();
        let (resolved, _) =
            tyc_resolve::resolve_module("<test>".to_owned(), &prep.python_source, &module);
        (resolved, prep.python_source)
    }

    #[test]
    fn completion_includes_visible_bindings_and_keywords() {
        let src = "\
def outer(a):
    def inner(b):
        return a + b
";
        let (resolved, preprocessed) = parse_resolved(src);
        // Position the cursor on the `return` line, at column 8 (inside the
        // statement, after the indent).
        let pos = Position {
            line: 2,
            character: 8,
        };
        let items = compute_completion_items(&resolved, &preprocessed, pos);
        let labels: std::collections::HashSet<String> =
            items.iter().map(|i| i.label.clone()).collect();
        // Bindings from the enclosing scopes are present:
        assert!(labels.contains("a"), "expected `a`: {labels:?}");
        assert!(labels.contains("b"), "expected `b`: {labels:?}");
        assert!(labels.contains("outer"), "expected `outer`: {labels:?}");
        assert!(labels.contains("inner"), "expected `inner`: {labels:?}");
        // Typhon keywords and a representative builtin:
        assert!(labels.contains("let"), "missing let keyword: {labels:?}");
        assert!(
            labels.contains("gather"),
            "missing gather keyword: {labels:?}"
        );
        assert!(
            labels.contains("print"),
            "missing print builtin: {labels:?}"
        );
    }

    #[test]
    fn completion_kinds_distinguish_function_and_value() {
        let src = "\
let name: str = \"hi\"
def greet():
    return name
";
        let (resolved, preprocessed) = parse_resolved(src);
        let pos = Position {
            line: 0,
            character: 0,
        };
        let items = compute_completion_items(&resolved, &preprocessed, pos);
        let greet = items.iter().find(|i| i.label == "greet").expect("greet");
        let name = items.iter().find(|i| i.label == "name").expect("name");
        assert_eq!(greet.kind, Some(CompletionItemKind::FUNCTION));
        // `let` bindings render as CONSTANT to differentiate from `mut`.
        assert_eq!(name.kind, Some(CompletionItemKind::CONSTANT));
    }

    // ── Phase 5: library / module member completion ──────────────────────

    #[test]
    fn extract_receiver_handles_trailing_dot() {
        // Cursor immediately after `os.` — no partial member typed yet.
        let text = "import os\n\ndef f() -> None:\n    os.\n";
        let offset = text.find("os.\n").unwrap() + "os.".len();
        let r = extract_member_access_receiver(text, offset);
        assert_eq!(r.as_deref(), Some("os"));
    }

    #[test]
    fn extract_receiver_handles_partial_member() {
        // Cursor in the middle of typing `os.getc|wd` — must still
        // return `os` as the receiver.
        let text = "import os\n\ndef f() -> None:\n    os.getc";
        let offset = text.len();
        let r = extract_member_access_receiver(text, offset);
        assert_eq!(r.as_deref(), Some("os"));
    }

    #[test]
    fn extract_receiver_handles_dotted_chain() {
        // `os.path.|` — receiver is the full dotted path.
        let text = "import os\n\ndef f() -> None:\n    os.path.";
        let offset = text.len();
        let r = extract_member_access_receiver(text, offset);
        assert_eq!(r.as_deref(), Some("os.path"));
    }

    #[test]
    fn extract_receiver_returns_none_without_dot() {
        // Plain identifier without a trailing dot is not a member access.
        let text = "def f() -> None:\n    print";
        let offset = text.len();
        let r = extract_member_access_receiver(text, offset);
        assert_eq!(r, None);
    }

    #[test]
    fn extract_from_import_module_after_import_keyword() {
        // `from os import |` — cursor right after the space following `import`.
        let text = "from os import ";
        let r = extract_from_import_module(text, text.len());
        assert_eq!(r.as_deref(), Some("os"));
    }

    #[test]
    fn extract_from_import_module_partial_member() {
        // `from os import get|` — cursor mid-typing the first member.
        let text = "from os import get";
        let r = extract_from_import_module(text, text.len());
        assert_eq!(r.as_deref(), Some("os"));
    }

    #[test]
    fn extract_from_import_module_after_comma() {
        // `from os import path, |` — additional member after a comma.
        let text = "from os import path, ";
        let r = extract_from_import_module(text, text.len());
        assert_eq!(r.as_deref(), Some("os"));
    }

    #[test]
    fn extract_from_import_module_dotted_module() {
        // Dotted module names like `urllib.parse` resolve correctly.
        let text = "from urllib.parse import ";
        let r = extract_from_import_module(text, text.len());
        assert_eq!(r.as_deref(), Some("urllib.parse"));
    }

    #[test]
    fn extract_from_import_module_immediately_after_import() {
        // `from os import|` — cursor flush against `import` (no trailing space).
        let text = "from os import";
        let r = extract_from_import_module(text, text.len());
        assert_eq!(r.as_deref(), Some("os"));
    }

    #[test]
    fn extract_from_import_module_returns_none_before_import_keyword() {
        // Cursor still inside the module name; the `import` keyword
        // hasn't been typed yet.
        let text = "from os ";
        let r = extract_from_import_module(text, text.len());
        assert_eq!(r, None);
    }

    #[test]
    fn extract_from_import_module_returns_none_for_plain_import() {
        // `import os` is not a from-import — no member completion to surface.
        let text = "import os";
        let r = extract_from_import_module(text, text.len());
        assert_eq!(r, None);
    }

    #[test]
    fn extract_from_import_module_returns_none_for_relative_import() {
        // `from .foo import …` is a relative import; we can't introspect it.
        let text = "from .foo import ";
        let r = extract_from_import_module(text, text.len());
        assert_eq!(r, None);
    }

    #[test]
    fn extract_from_import_module_returns_none_after_semicolon() {
        // `from os import path; x = <cursor>` — cursor walked past the
        // statement terminator. Open-code completion (not from-import)
        // is the right answer here.
        let text = "from os import path; x = ";
        let r = extract_from_import_module(text, text.len());
        assert_eq!(r, None);
    }

    #[test]
    fn extract_from_import_module_returns_none_inside_comment() {
        // `from os import path  # <cursor>` — cursor inside the
        // trailing comment, not the import list.
        let text = "from os import path  # ";
        let r = extract_from_import_module(text, text.len());
        assert_eq!(r, None);
    }

    #[test]
    fn extract_from_import_module_returns_none_outside_import() {
        // Plain code — cursor isn't inside a from-import.
        let text = "def f() -> None:\n    pass\n";
        let r = extract_from_import_module(text, text.len());
        assert_eq!(r, None);
    }

    #[test]
    fn extract_receiver_rejects_leading_dot() {
        // `.foo` — no receiver to complete against.
        let text = "def f() -> None:\n    .";
        let offset = text.len();
        let r = extract_member_access_receiver(text, offset);
        assert_eq!(r, None);
    }

    #[test]
    fn completion_on_empty_resolved_module_does_not_panic() {
        // When the buffer is mid-edit and doesn't parse, the Salsa
        // `resolved_module_arc` query returns a default `ResolvedModule`
        // with no scopes. Indexing into that vec used to panic, crashing
        // the completion handler and surfacing as a "No suggestions"
        // popup in the editor. The path must now return cleanly — what
        // gets returned depends on the cursor context:
        //
        //   * In open code (no member access, no from-import): keywords
        //     + builtins so the user has something useful to pick from.
        //   * Inside a `from <unknown> import …`: empty, because
        //     suggesting `let` or `print` here would be misleading.
        let empty = tyc_resolve::ResolvedModule::default();
        // Open-code mid-keystroke — still gets keywords + builtins.
        let items = compute_completion_items(
            &empty,
            "def foo() -> \n",
            Position {
                line: 0,
                character: 13,
            },
        );
        let labels: std::collections::HashSet<String> =
            items.iter().map(|i| i.label.clone()).collect();
        assert!(labels.contains("let"), "missing `let` keyword: {labels:?}");
        assert!(
            labels.contains("print"),
            "missing `print` builtin: {labels:?}"
        );

        // In a from-import with no introspection available — empty is
        // correct, the important guarantee is *no panic*.
        let prefix = "from agent_framework import ";
        let src = format!("{prefix}\n");
        let _ = compute_completion_items(
            &empty,
            &src,
            Position {
                line: 0,
                character: prefix.len() as u32,
            },
        );
    }

    // The completion path is fed by the LSP every keystroke — including
    // when the buffer is mid-edit and doesn't parse. The Salsa
    // `resolved_module_arc` query returns an empty `ResolvedModule` on
    // parse failure (verified by `resolver_parse_error_yields_default_module`),
    // so our completion-after-`.` tests need source that parses cleanly.
    // We achieve that by ending the partial member with a real
    // identifier (e.g. `os.getcwd`) and positioning the cursor right
    // after the dot. The receiver-extraction logic uses byte offsets,
    // so the trailing identifier is invisible to the resolution path.

    #[test]
    fn completion_after_import_module_returns_stub_members() {
        // `import os` then `os.<cursor>getcwd()` — placing the cursor
        // immediately after the dot should surface curated stub members
        // (`getcwd`, `listdir`, …) and *only* those.
        let src = "\
import os

def f() -> None:
    let _x: str = os.getcwd()
";
        let (resolved, preprocessed) = parse_resolved(src);
        let needle = "os.getcwd()";
        let offset = preprocessed.find(needle).unwrap() + "os.".len();
        let position = byte_to_position(&preprocessed, offset);
        let items = compute_completion_items(&resolved, &preprocessed, position);
        let labels: std::collections::HashSet<String> =
            items.iter().map(|i| i.label.clone()).collect();
        assert!(labels.contains("getcwd"), "expected `getcwd`: {labels:?}");
        assert!(labels.contains("listdir"), "expected `listdir`: {labels:?}");
        assert!(labels.contains("path"), "expected `path`: {labels:?}");
        // Make sure we did NOT return the open-completion grab-bag — the
        // function `f`, the keyword `let`, and the builtin `print` should
        // all be absent because we're in a member-access context.
        assert!(!labels.contains("f"), "scope binding leaked: {labels:?}");
        assert!(!labels.contains("let"), "keyword leaked: {labels:?}");
        assert!(!labels.contains("print"), "builtin leaked: {labels:?}");
    }

    #[test]
    fn completion_after_alias_resolves_through_import_info() {
        // `import collections as c` then `c.deque(...)` — the alias `c`
        // must resolve back to the `collections` module so the popup
        // surfaces `deque`, `Counter`, etc.
        let src = "\
import collections as c

def f() -> None:
    let _q = c.deque()
";
        let (resolved, preprocessed) = parse_resolved(src);
        let needle = "c.deque()";
        let offset = preprocessed.find(needle).unwrap() + "c.".len();
        let position = byte_to_position(&preprocessed, offset);
        let items = compute_completion_items(&resolved, &preprocessed, position);
        let labels: std::collections::HashSet<String> =
            items.iter().map(|i| i.label.clone()).collect();
        assert!(labels.contains("deque"), "expected `deque`: {labels:?}");
        assert!(labels.contains("Counter"), "expected `Counter`: {labels:?}");
    }

    #[test]
    fn completion_after_submodule_dot_resolves_combined_path() {
        // `import os` then `os.path.join(...)` — joining the import root
        // with the dotted tail must look up `os.path` in the stub
        // registry and return *its* members.
        let src = "\
import os

def f() -> None:
    let _p: str = os.path.join(\"a\", \"b\")
";
        let (resolved, preprocessed) = parse_resolved(src);
        let needle = "os.path.join";
        let offset = preprocessed.find(needle).unwrap() + "os.path.".len();
        let position = byte_to_position(&preprocessed, offset);
        let items = compute_completion_items(&resolved, &preprocessed, position);
        let labels: std::collections::HashSet<String> =
            items.iter().map(|i| i.label.clone()).collect();
        assert!(labels.contains("join"), "expected `join`: {labels:?}");
        assert!(labels.contains("exists"), "expected `exists`: {labels:?}");
        // Crucially the parent module's members must NOT leak in.
        assert!(
            !labels.contains("getcwd"),
            "os leaked into os.path: {labels:?}"
        );
    }

    #[test]
    fn completion_after_unknown_receiver_returns_empty() {
        // `randomname.foo` where `randomname` isn't a known binding —
        // we return an empty list rather than the open-completion menu.
        // Emitting `let`, `print`, etc. as member completions after a
        // dot would be misleading.
        let src = "\
def f() -> None:
    let _x = randomname.foo()
";
        let (resolved, preprocessed) = parse_resolved(src);
        let needle = "randomname.foo";
        let offset = preprocessed.find(needle).unwrap() + "randomname.".len();
        let position = byte_to_position(&preprocessed, offset);
        let items = compute_completion_items(&resolved, &preprocessed, position);
        assert!(
            items.is_empty(),
            "expected empty completion, got: {labels:?}",
            labels = items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn completion_inside_from_import_surfaces_module_members() {
        // `from os import <cursor>` should surface `os`'s members
        // (e.g. `getcwd`) — exactly the case the user reported with
        // `from agent_framework import <cursor>`. Mid-edit the source
        // doesn't parse (Python's grammar requires at least one
        // imported name), so the LSP receives a default
        // `ResolvedModule` — the from-import detector reads from the
        // raw preprocessed text, not the AST, so it still fires.
        // Introspect is `None` here, so completion falls back to the
        // curated stdlib stubs (which include `os`).
        let preprocessed = "from os import \n";
        let resolved = tyc_resolve::ResolvedModule::default();
        let prefix = "from os import ";
        let pos = Position {
            line: 0,
            character: prefix.len() as u32,
        };
        let items = compute_completion_items(&resolved, preprocessed, pos);
        let labels: std::collections::HashSet<String> =
            items.iter().map(|i| i.label.clone()).collect();
        assert!(
            labels.contains("getcwd"),
            "from-import completion should surface `getcwd` from `os` stubs; got {labels:?}"
        );
        // Keywords / builtins must NOT leak into the import list —
        // they aren't valid imports.
        assert!(
            !labels.contains("let"),
            "Typhon keyword `let` should not appear in a from-import list"
        );
        assert!(
            !labels.contains("print"),
            "builtin `print` should not appear in a from-import list"
        );
    }

    #[test]
    fn fixup_resolves_source_with_trailing_dot() {
        // Simulates the mid-keystroke state: the file ends in `os.` and
        // doesn't parse cleanly. `try_fixup_and_resolve` should insert a
        // placeholder and re-resolve, exposing the `import os` binding.
        let src = "\
import os

def f() -> None:
    os.
";
        let prep = tyc_syntax::preprocess::preprocess(src);
        let offset = prep.python_source.find("    os.").unwrap() + "    os.".len();
        let position = byte_to_position(&prep.python_source, offset);
        let (patched, resolved) = try_fixup_and_resolve(&prep.python_source, position)
            .expect("fixup should succeed for `os.<cursor>`");
        assert!(
            !resolved.scopes.is_empty(),
            "expected non-empty scopes after fixup"
        );
        // Sanity: the placeholder is at the cursor position, not before.
        let cursor_in_patched = position_to_byte(&patched, position);
        assert_eq!(
            &patched[cursor_in_patched..cursor_in_patched + 1],
            "X",
            "placeholder should sit at the cursor offset"
        );
        // Running completion on the patched source should now surface the
        // stub members — `os` is visible through resolution.
        let items = compute_completion_items(&resolved, &patched, position);
        let labels: std::collections::HashSet<String> =
            items.iter().map(|i| i.label.clone()).collect();
        assert!(labels.contains("getcwd"), "expected `getcwd`: {labels:?}");
    }

    #[test]
    fn fixup_returns_none_when_not_after_dot() {
        // No trailing `.` at the cursor → no fix-up worth attempting.
        let src = "let _ = 1\n";
        let prep = tyc_syntax::preprocess::preprocess(src);
        let position = byte_to_position(&prep.python_source, prep.python_source.len());
        assert!(try_fixup_and_resolve(&prep.python_source, position).is_none());
    }

    #[test]
    fn introspection_takes_precedence_over_curated_stubs() {
        // When a venv-introspection callback is supplied, its result
        // wins over the curated stub registry. Verify by handing in a
        // fake callback that returns a single member with a distinctive
        // name and confirming it surfaces (instead of the curated `os`
        // member list).
        let src = "\
import os

def f() -> None:
    let _x = os.getcwd()
";
        let (resolved, preprocessed) = parse_resolved(src);
        let offset = preprocessed.find("os.getcwd").unwrap() + "os.".len();
        let position = byte_to_position(&preprocessed, offset);

        // Sanity: with no callback we hit the curated stubs path.
        let baseline = compute_completion_items(&resolved, &preprocessed, position);
        assert!(
            baseline.iter().any(|i| i.label == "getcwd"),
            "curated baseline should include getcwd"
        );

        // Now provide a callback that ignores the curated table and
        // returns a synthetic member.
        let stubbed = |module: &str| -> Option<Vec<CompletionItem>> {
            assert_eq!(module, "os", "callback should see resolved module path");
            Some(vec![CompletionItem {
                label: "from_introspection".to_owned(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some("synthetic() -> None".to_owned()),
                ..Default::default()
            }])
        };
        let items = compute_completion_items_with_introspection(
            &resolved,
            &preprocessed,
            position,
            Some(&stubbed),
        );
        let labels: std::collections::HashSet<String> =
            items.iter().map(|i| i.label.clone()).collect();
        assert!(
            labels.contains("from_introspection"),
            "expected introspection result: {labels:?}"
        );
        // Curated `getcwd` must NOT appear when introspection succeeded —
        // we don't want to mix the two sources or surface stale entries.
        assert!(
            !labels.contains("getcwd"),
            "introspection should suppress curated stub: {labels:?}"
        );
    }

    #[test]
    fn introspection_failure_falls_back_to_curated_stubs() {
        // If the introspection callback returns None (module not in
        // the venv, subprocess failure, etc.), we drop to the curated
        // stub table so the user still gets useful suggestions.
        let src = "\
import os

def f() -> None:
    let _x = os.getcwd()
";
        let (resolved, preprocessed) = parse_resolved(src);
        let offset = preprocessed.find("os.getcwd").unwrap() + "os.".len();
        let position = byte_to_position(&preprocessed, offset);

        let always_none = |_module: &str| -> Option<Vec<CompletionItem>> { None };
        let items = compute_completion_items_with_introspection(
            &resolved,
            &preprocessed,
            position,
            Some(&always_none),
        );
        let labels: std::collections::HashSet<String> =
            items.iter().map(|i| i.label.clone()).collect();
        assert!(
            labels.contains("getcwd"),
            "expected curated fallback: {labels:?}"
        );
    }

    #[test]
    fn completion_member_items_carry_signature_detail() {
        // The curated `detail` line from the stub should flow through as
        // the LSP `CompletionItem.detail` field — that's what the editor
        // renders next to the member name in the popup.
        let src = "\
import json

def f() -> None:
    let _v = json.loads(\"{}\")
";
        let (resolved, preprocessed) = parse_resolved(src);
        let needle = "json.loads";
        let offset = preprocessed.find(needle).unwrap() + "json.".len();
        let position = byte_to_position(&preprocessed, offset);
        let items = compute_completion_items(&resolved, &preprocessed, position);
        let loads = items
            .iter()
            .find(|i| i.label == "loads")
            .expect("loads completion present");
        let detail = loads.detail.as_deref().expect("loads has a signature");
        assert!(
            detail.contains("loads(s: str)"),
            "expected loads signature, got: {detail}"
        );
    }

    // ── Phase 4: code actions ────────────────────────────────────────────

    fn uri(s: &str) -> Uri {
        s.parse().unwrap()
    }

    fn unused_import_diag(line: u32, character: u32, length: u32) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position { line, character },
                end: Position {
                    line,
                    character: character + length,
                },
            },
            severity: Some(DiagnosticSeverity::WARNING),
            code: Some(NumberOrString::String("tyc::unused_import".to_string())),
            code_description: None,
            source: Some("tyc".into()),
            message: "unused import `os`".to_string(),
            related_information: None,
            tags: None,
            data: None,
        }
    }

    #[test]
    fn code_action_offers_remove_for_unused_import() {
        let u = uri("file:///tmp/foo.ty");
        let text = "import os\nlet x: int = 1\n";
        let diag = unused_import_diag(0, 7, 2);
        let actions = compute_code_actions(&u, text, &[diag]);
        assert_eq!(actions.len(), 1, "expected one action");
        let CodeActionOrCommand::CodeAction(action) = &actions[0] else {
            panic!("expected CodeAction variant");
        };
        assert_eq!(action.title, "Remove unused import");
        assert_eq!(action.kind, Some(CodeActionKind::QUICKFIX));
        assert_eq!(action.is_preferred, Some(true));

        let edit = action.edit.as_ref().expect("edit set");
        let changes = edit.changes.as_ref().expect("changes set");
        let edits = changes.get(&u).expect("edits for our uri");
        assert_eq!(edits.len(), 1);
        // The edit must span the entire `import os\n` line so deleting it
        // removes the trailing newline too.
        assert_eq!(edits[0].new_text, "");
        assert_eq!(edits[0].range.start.line, 0);
        assert_eq!(edits[0].range.start.character, 0);
        assert_eq!(edits[0].range.end.line, 1);
        assert_eq!(edits[0].range.end.character, 0);
    }

    #[test]
    fn code_action_skips_multi_import_line() {
        // Regression for the Copilot/gemini review: `import os, sys`
        // with only `os` flagged must NOT produce a whole-line delete
        // — that would also drop the still-used `sys` import.
        let u = uri("file:///tmp/foo.ty");
        let text = "import os, sys\nlet x: int = 1\n";
        let diag = unused_import_diag(0, 7, 2);
        let actions = compute_code_actions(&u, text, &[diag]);
        assert!(
            actions.is_empty(),
            "multi-import line must NOT produce a whole-line quick-fix"
        );
    }

    #[test]
    fn code_action_skips_chained_statement_line() {
        // Regression for the gemini review: a line that chains a second
        // statement via `;` would have its second statement deleted
        // along with the import. Skip the quick-fix to stay safe.
        let u = uri("file:///tmp/foo.ty");
        let text = "import os; x = 1\n";
        let diag = unused_import_diag(0, 7, 2);
        let actions = compute_code_actions(&u, text, &[diag]);
        assert!(
            actions.is_empty(),
            "chained-statement line must NOT produce a whole-line quick-fix"
        );
    }

    #[test]
    fn code_action_offers_fix_for_simple_import_with_comment() {
        // Trailing `# comment` on the import line is OK to drop along
        // with the import — the comment was documenting the now-gone
        // import.
        let u = uri("file:///tmp/foo.ty");
        let text = "import os  # legacy\nlet x: int = 1\n";
        let diag = unused_import_diag(0, 7, 2);
        let actions = compute_code_actions(&u, text, &[diag]);
        assert_eq!(actions.len(), 1, "simple import + comment should fix");
    }

    #[test]
    fn is_safe_single_import_line_classifications() {
        assert!(is_safe_single_import_line("import os"));
        assert!(is_safe_single_import_line("import os as o"));
        assert!(is_safe_single_import_line("from m import x"));
        assert!(is_safe_single_import_line("from m import x as y"));
        assert!(is_safe_single_import_line("  import os  "));
        assert!(is_safe_single_import_line("import os  # legacy"));
        // Unsafe shapes:
        assert!(!is_safe_single_import_line("import os, sys"));
        assert!(!is_safe_single_import_line("from m import x, y"));
        assert!(!is_safe_single_import_line("import os; x = 1"));
        assert!(!is_safe_single_import_line(""));
        assert!(!is_safe_single_import_line("x = 1"));
    }

    #[test]
    fn nth_line_content_handles_last_line_no_newline() {
        let text = "first\nsecond";
        assert_eq!(nth_line_content(text, 0), "first");
        assert_eq!(nth_line_content(text, 1), "second");
        assert_eq!(nth_line_content(text, 5), "");
    }

    #[test]
    fn code_action_ignores_unrelated_diagnostics() {
        let u = uri("file:///tmp/foo.ty");
        let text = "let x: int = 1\n";
        let mut diag = unused_import_diag(0, 0, 3);
        diag.code = Some(NumberOrString::String("tyc::type_mismatch".to_string()));
        let actions = compute_code_actions(&u, text, &[diag]);
        assert!(
            actions.is_empty(),
            "non-matching code must not produce actions"
        );
    }

    #[test]
    fn whole_line_range_handles_last_line_without_newline() {
        let text = "first\nsecond";
        let range = whole_line_range(text, 1);
        assert_eq!(range.start.line, 1);
        assert_eq!(range.start.character, 0);
        // Without a trailing newline the end position must clamp to the last
        // character on the line rather than overshooting to the next line.
        assert_eq!(range.end.line, 1);
        assert_eq!(range.end.character, 6);
    }

    #[test]
    fn render_hover_handles_unresolved_symbol() {
        let symbol = SymbolAtOffset {
            name: "mystery".to_owned(),
            span: (0, 7),
            definition: None,
            is_definition: false,
        };
        let body = render_hover(&symbol);
        assert!(body.contains("unresolved"), "got: {body}");
    }

    // ── diagnostic_source selection ───────────────────────────────────────────

    #[test]
    fn diagnostic_source_returns_original_for_invalid_question_op() {
        let original = "let x = y?";
        let preprocessed = "x = y_result";
        let err = TycError::invalid_question_op("bad use of ?", "f.ty", original, 8, 2);
        assert_eq!(
            diagnostic_source(&err, original, preprocessed),
            original,
            "InvalidQuestionOp diagnostics are anchored to the original Typhon source"
        );
    }

    #[test]
    fn diagnostic_source_returns_preprocessed_for_type_mismatch() {
        let original = "let x: int = \"hi\"";
        let preprocessed = "x: int = \"hi\"";
        let err = TycError::type_mismatch("int", "str", "f.ty", preprocessed, 0, 1);
        assert_eq!(
            diagnostic_source(&err, original, preprocessed),
            preprocessed,
            "TypeMismatch diagnostics are anchored to the preprocessed source"
        );
    }

    #[test]
    fn diagnostic_source_returns_preprocessed_for_unknown_name() {
        let original = "let y: str = z";
        let preprocessed = "y: str = z";
        let err = TycError::unknown_name("z", "f.ty", preprocessed, 9, 1);
        assert_eq!(
            diagnostic_source(&err, original, preprocessed),
            preprocessed,
            "UnknownName diagnostics are anchored to the preprocessed source"
        );
    }

    #[test]
    fn diagnostic_source_returns_original_for_lazy_usage() {
        // validate_lazy_usage runs against the raw Typhon source before any
        // sugar expansion, so its byte offsets refer to the editor buffer.
        let original = "lazy from os import path";
        let preprocessed = "from os import path";
        let err = TycError::lazy_usage("unsupported lazy form", "f.ty", original, 0, 24);
        assert_eq!(
            diagnostic_source(&err, original, preprocessed),
            original,
            "LazyUsage diagnostics are anchored to the original (pre-expansion) source"
        );
    }

    // ── tyc_error_to_lsp conversion ───────────────────────────────────────────

    #[test]
    fn tyc_error_to_lsp_returns_none_for_span_less_io_error() {
        let err = TycError::io("f.ty", &std::io::Error::other("disk full"));
        let result = tyc_error_to_lsp(&err, "source text", DiagnosticSeverity::ERROR);
        assert!(
            result.is_none(),
            "I/O errors have no source span so conversion should return None"
        );
    }

    #[test]
    fn tyc_error_to_lsp_returns_none_for_span_less_comptime_error() {
        let err = TycError::comptime("PORT", "missing env var");
        let result = tyc_error_to_lsp(&err, "source text", DiagnosticSeverity::ERROR);
        assert!(
            result.is_none(),
            "Comptime errors have no source span so conversion should return None"
        );
    }

    #[test]
    fn tyc_error_to_lsp_converts_type_mismatch_to_diagnostic() {
        let src = "let x: int = \"hi\"";
        let err = TycError::type_mismatch("int", "str", "f.ty", src, 0, 3);
        let result = tyc_error_to_lsp(&err, src, DiagnosticSeverity::ERROR);
        assert!(
            result.is_some(),
            "TypeMismatch should produce an LSP diagnostic"
        );
        let d = result.unwrap();
        assert_eq!(d.range.start.line, 0, "span starts on line 0");
        assert_eq!(d.range.start.character, 0, "span starts at column 0");
        assert_eq!(d.range.end.character, 3, "span end reflects length 3");
        assert!(
            d.message.contains("int") && d.message.contains("str"),
            "diagnostic message should mention both types"
        );
    }

    #[test]
    fn tyc_error_to_lsp_preserves_error_code() {
        let src = "let x: int = \"hi\"";
        let err = TycError::type_mismatch("int", "str", "f.ty", src, 0, 1);
        let d = tyc_error_to_lsp(&err, src, DiagnosticSeverity::ERROR).unwrap();
        assert!(
            matches!(&d.code, Some(NumberOrString::String(s)) if s == "tyc::type_mismatch"),
            "LSP diagnostic should carry the tyc::type_mismatch error code"
        );
    }

    #[test]
    fn tyc_error_to_lsp_sets_source_field() {
        let src = "let x: int = \"hi\"";
        let err = TycError::type_mismatch("int", "str", "f.ty", src, 0, 1);
        let d = tyc_error_to_lsp(&err, src, DiagnosticSeverity::WARNING).unwrap();
        assert_eq!(
            d.source.as_deref(),
            Some("tyc"),
            "LSP diagnostic source should be 'tyc'"
        );
    }

    #[test]
    fn tyc_error_to_lsp_multiline_span_maps_correctly() {
        // Span that starts at byte 4 (second line) in a two-line source.
        let src = "abc\ndef";
        let err = TycError::unknown_name("def", "f.ty", src, 4, 3);
        let d = tyc_error_to_lsp(&err, src, DiagnosticSeverity::ERROR).unwrap();
        assert_eq!(
            d.range.start.line, 1,
            "span should start on line 1 (second line)"
        );
        assert_eq!(d.range.start.character, 0, "span should start at column 0");
    }

    // ── resolved_module Salsa query ───────────────────────────────────────────

    #[test]
    fn resolved_module_query_exposes_bindings() {
        // `resolved_module_arc` must return a module with the declared binding.
        let db = TycDatabase::new();
        let file = SourceFile::new(&db, "test.ty".into(), "let x: int = 1\n".into());
        let resolved = resolved_module_arc(&db, file);
        let names: Vec<String> = resolved
            .module_scope()
            .bindings
            .iter()
            .map(|b| b.name.clone())
            .collect();
        assert!(
            names.contains(&"x".to_owned()),
            "resolved_module should expose the let binding; got {names:?}"
        );
    }

    #[test]
    fn resolved_module_query_unchanged_text_is_cached() {
        // After the first call, a second call with unchanged text must return
        // the same Arc (Salsa cache hit → pointer equality).
        let db = TycDatabase::new();
        let file = SourceFile::new(&db, "test.ty".into(), "let x: int = 1\n".into());
        let first = resolved_module_arc(&db, file);
        let second = resolved_module_arc(&db, file);
        assert!(
            Arc::ptr_eq(&first, &second),
            "unchanged file should return the same Arc from the Salsa cache"
        );
    }
}
