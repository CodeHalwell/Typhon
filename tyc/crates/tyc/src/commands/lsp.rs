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
    /// Severity threshold for status messages the LSP forwards to the editor
    /// via the `window/logMessage` channel. One of `error`, `warn`, `info`,
    /// `debug`. Defaults to `info`.
    #[arg(long, default_value = "info")]
    pub log_level: String,
}

pub fn run(args: LspArgs) -> Result<()> {
    let level = tyc_lsp::LogLevel::parse(&args.log_level);
    tyc_lsp::run_stdio(level);
    Ok(())
}
