//! Language Server Protocol backend for Typhon.
//!
//! Implements a minimal `tower-lsp-server` backend that re-runs the Phase-1
//! type-check pipeline whenever a file is opened or changed and publishes
//! the resulting diagnostics back to the editor. Hover currently returns a
//! placeholder; richer responses arrive once the resolver and type checker
//! expose query-by-position interfaces.

use std::sync::Arc;

use miette::{Diagnostic as MietteDiagnostic, LabeledSpan};
use tokio::sync::Mutex;
use tower_lsp_server::ls_types::{
    Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, Hover, HoverContents, HoverParams, HoverProviderCapability,
    InitializeParams, InitializeResult, InitializedParams, MarkedString, MessageType,
    NumberOrString, Position, Range, ServerCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncKind, Uri,
};
use tower_lsp_server::{jsonrpc, Client, LanguageServer, LspService, Server};
use tyc_db::{check_file, TycDatabase};
use tyc_diagnostics::TycError;
use tyc_syntax::preprocess::{expand_pipes, expand_question_ops, expand_with_chains, preprocess};

/// The Typhon LSP backend. Holds a single shared salsa database and the
/// `Client` handle used to send notifications back to the editor.
///
/// The database is wrapped in [`Arc<tokio::sync::Mutex<_>>`] so the
/// `check_file` call can run on a blocking executor thread without
/// pinning the async runtime — concurrent `hover` and `shutdown`
/// requests stay responsive while a file is being checked.
pub struct Backend {
    client: Client,
    db: Arc<Mutex<TycDatabase>>,
    log_level: LogLevel,
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
        let path = uri.as_str().to_owned();
        let db = Arc::clone(&self.db);
        let text_for_check = text.clone();
        let path_for_check = path.clone();

        let result = tokio::task::spawn_blocking(move || {
            // Compute the source string the diagnostics will reference for
            // their byte offsets. After preprocessing (`val`/`var` stripping,
            // `?`/`|>`/`with`-chain expansion) the byte layout changes — most
            // diagnostics from the resolver and type-checker are anchored to
            // the *preprocessed* source, not the editor buffer. We therefore
            // run the same expansion pipeline locally and publish ranges
            // relative to that text. The exception is the `?`-operator
            // validator, which reports against the original Typhon source;
            // those diagnostics are handled via their explicit byte offsets
            // when emitted by `check_file` and continue to map cleanly.
            let expanded = expand_question_ops(&expand_pipes(&expand_with_chains(&text_for_check)));
            let prep = preprocess(&expanded);
            let diags = {
                // Hold the mutex only for the duration of the salsa call.
                let mut db = db.blocking_lock();
                check_file(&mut db, path_for_check, text_for_check)
            };
            (diags, prep.python_source)
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
        self.client
            .publish_diagnostics(params.text_document.uri, Vec::new(), None)
            .await;
    }

    async fn hover(&self, _params: HoverParams) -> jsonrpc::Result<Option<Hover>> {
        // Stub: hover-by-position over the resolved type table will be wired
        // here when the resolver exposes a salsa query that takes a (file,
        // position) pair and returns the symbol at that point.
        Ok(Some(Hover {
            contents: HoverContents::Scalar(MarkedString::String(
                "Typhon — hover details land with structural typing in Phase 3".to_owned(),
            )),
            range: None,
        }))
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
        });
        Server::new(stdin, stdout, socket).serve(service).await;
    });
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
/// source because that's what the parser and resolver see after `val`/`var`
/// stripping and sugar expansion. Selecting the correct reference text per
/// diagnostic variant keeps published LSP ranges aligned with the editor
/// buffer instead of drifting by a column or two after `val` is removed.
fn diagnostic_source<'a>(err: &TycError, original: &'a str, preprocessed: &'a str) -> &'a str {
    match err {
        TycError::InvalidQuestionOp { .. } => original,
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
}
