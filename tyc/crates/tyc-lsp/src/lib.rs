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
    MarkedString, MessageType, NumberOrString, OneOf, Position, Range, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit, Uri, WorkspaceEdit,
};
use tower_lsp_server::{jsonrpc, Client, LanguageServer, LspService, Server};
use tyc_db::{check_source_file, preprocessed_text, resolved_module_arc, SourceFile, TycDatabase};
use tyc_diagnostics::TycError;
use tyc_resolve::{
    BindingKind, ClassKind, ImportInfo, Mutability, ResolveOptions, ResolvedModule, SymbolAtOffset,
};

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
    introspection:
        Arc<Mutex<HashMap<std::path::PathBuf, Arc<Mutex<venv_introspect::IntrospectionCache>>>>>,
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
            docs.insert(uri_str, source_file);
        }

        let result = tokio::task::spawn_blocking(move || {
            // Hold the mutex only for the duration of the salsa call.
            let mut db = db.blocking_lock();
            #[allow(clippy::explicit_auto_deref)]
            let diags = check_source_file(&mut *db, source_file);
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

        self.client.publish_diagnostics(uri, out, version).await;
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

        let body = render_hover(&symbol);
        let range = Some(Range {
            start: byte_to_position(&preprocessed, symbol.span.0),
            end: byte_to_position(&preprocessed, symbol.span.1),
        });
        Ok(Some(Hover {
            contents: HoverContents::Scalar(MarkedString::String(body)),
            range,
        }))
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
        // Resolve the project root once so we can wire venv
        // introspection through to the completion path. The cache
        // shell-launches `.venv/bin/python` on miss, so we run it
        // off the runtime thread to keep async handlers responsive.
        let cache_handle = self.introspection_cache_for(&uri).await;
        let introspect_owned: Option<
            Box<dyn Fn(&str) -> Option<Vec<CompletionItem>> + Send + Sync>,
        > = match cache_handle {
            Some((root, cache)) => Some(Box::new(move |module: &str| {
                // `members` may block on a subprocess; we're inside an
                // async fn but the rest of the LSP isn't holding
                // anything that needs us to yield, so a short blocking
                // lock here is fine. The cache itself is process-wide
                // synchronised so a second concurrent completion for
                // the same module just blocks on the prior result.
                let mut guard = cache.blocking_lock();
                guard
                    .members(&root, module)
                    .map(|m| introspected_members_to_completion(&m))
            })),
            None => None,
        };
        let items = compute_completion_items_with_introspection(
            &resolved,
            &preprocessed,
            position,
            introspect_owned
                .as_ref()
                .map(|b| b.as_ref() as &dyn Fn(&str) -> Option<Vec<CompletionItem>>),
        );
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
        Arc<Mutex<venv_introspect::IntrospectionCache>>,
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
                Arc::new(Mutex::new(
                    venv_introspect::IntrospectionCache::for_project_root(&root),
                ))
            })
            .clone();
        Some((root, cache))
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
///    scope chain; if it bottoms out at an `import` binding, we look up
///    the imported module in [`stdlib_stubs`] and return its members.
///    Builtin-typed `let` bindings (`let xs: list[int] = []`) also
///    contribute their type's method list.
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
    introspect: Option<&dyn Fn(&str) -> Option<Vec<CompletionItem>>>,
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
        &parsed.syntax(),
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

/// Resolve `receiver` against the scope visible at `offset` and, if it
/// bottoms out at a known import, return the matching stub members.
///
/// Resolution order:
///
/// 1. **Dotted module path** — `os.path`, `urllib.parse`: try
///    [`stdlib_stubs::lookup`] on the full string first, in case it
///    matches a curated submodule entry directly. This handles the
///    case where the receiver is *already* a fully-qualified module
///    name (e.g. the user typed it out without an alias).
/// 2. **Imported alias** — `np` from `import numpy as np`: walk the
///    binding's `ImportInfo` back to the original module name and look
///    that up. For dotted receivers (`np.linalg.`), the alias resolves
///    only the head; the trailing segments are appended onto the
///    `ImportInfo.module` path before lookup.
///
/// Type-driven completion (e.g. `let xs: list[int] = []; xs.` →
/// list-method members) is a planned follow-up and would require
/// either (a) running the type checker at LSP completion time, or
/// (b) walking the binding's declaration site to read its annotation
/// text. Neither is wired today; the stub registry path covers the
/// "import a stdlib module and want to see its surface" case which is
/// the bulk of the autocomplete value.
fn member_completion_items(
    resolved: &ResolvedModule,
    receiver: &str,
    offset: usize,
    introspect: Option<&dyn Fn(&str) -> Option<Vec<CompletionItem>>>,
) -> Option<Vec<CompletionItem>> {
    // Try every plausible interpretation of `receiver` in order:
    //   1. Receiver is literally a dotted module name (`os.path`).
    //   2. Receiver's head is an imported alias; rejoin with the tail.
    // For each interpretation, prefer venv introspection (the user's
    // *actual* installed packages) before falling back to the curated
    // stub registry.
    let try_resolve = |module: &str| -> Option<Vec<CompletionItem>> {
        if let Some(cb) = introspect {
            if let Some(items) = cb(module) {
                return Some(items);
            }
        }
        stdlib_stubs::lookup(module).map(stub_members_to_completion)
    };
    // Interpretation 1: the receiver text *is* the module path.
    if let Some(items) = try_resolve(receiver) {
        return Some(items);
    }
    // Interpretation 2: head resolves through the scope chain to an
    // imported module; tail (if any) extends the module path.
    let (head, tail) = match receiver.split_once('.') {
        Some((h, t)) => (h, Some(t)),
        None => (receiver, None),
    };
    let scope_id = resolved.scope_at_offset(offset);
    let binding = resolved.lookup(scope_id, head).map(|(b, _)| b)?;
    if binding.kind != BindingKind::Import {
        return None;
    }
    let info = binding.import_info.as_ref()?;
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
    try_resolve(&module_path)
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
    fn extract_receiver_rejects_leading_dot() {
        // `.foo` — no receiver to complete against.
        let text = "def f() -> None:\n    .";
        let offset = text.len();
        let r = extract_member_access_receiver(text, offset);
        assert_eq!(r, None);
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
