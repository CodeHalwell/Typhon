//! `tyc lsp` — run as a Language Server on stdio.
//!
//! Spawns the [`tyc_lsp`] backend on the current thread; the editor talks to
//! it over JSON-RPC framed on stdin/stdout. Diagnostics are re-published on
//! every `did_open` and `did_change`.

use clap::Args;
use miette::Result;

/// Arguments for `tyc lsp`.
#[derive(Args, Debug)]
pub struct LspArgs {
    /// Log level for the LSP server (`error`, `warn`, `info`, `debug`).
    ///
    /// Accepted today for editor compatibility; the backend uses the
    /// `client.log_message` channel for status messages regardless.
    #[arg(long, default_value = "info")]
    pub log_level: String,
}

pub fn run(_args: LspArgs) -> Result<()> {
    tyc_lsp::run_stdio();
    Ok(())
}
