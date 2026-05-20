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

    /// Accepted for compatibility with `vscode-languageclient`, which
    /// unconditionally appends `--stdio` when started with
    /// `TransportKind.stdio`. We only support stdio today, so the flag
    /// is a no-op — rejecting it would make the language client fail to
    /// start with `unexpected argument '--stdio'`.
    #[arg(long, hide = true)]
    pub stdio: bool,
}

pub fn run(args: LspArgs) -> Result<()> {
    let level = tyc_lsp::LogLevel::parse(&args.log_level);
    tyc_lsp::run_stdio(level);
    Ok(())
}
