//! `tyc lsp` — run as a Language Server on stdio.

use clap::Args;
use miette::Result;

/// Arguments for `tyc lsp`.
#[derive(Args, Debug)]
pub struct LspArgs {
    /// Log level for the LSP server (`error`, `warn`, `info`, `debug`).
    #[arg(long, default_value = "info")]
    pub log_level: String,
}

pub fn run(_args: LspArgs) -> Result<()> {
    // Phase 2 stub: tower-lsp-server backend will be wired here.
    eprintln!("tyc lsp: Language Server not yet implemented (Phase 2+)");
    Ok(())
}
