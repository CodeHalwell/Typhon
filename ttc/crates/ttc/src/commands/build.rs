//! `ttc build` — full compilation pipeline.

use std::path::PathBuf;

use clap::Args;
use miette::Result;

/// Arguments for `ttc build`.
#[derive(Args, Debug)]
pub struct BuildArgs {
    /// Project directory (defaults to current directory).
    #[arg(value_name = "PATH", default_value = ".")]
    pub path: PathBuf,

    /// Write output to this directory instead of the configured `out` dir.
    #[arg(long, short = 'o', value_name = "DIR")]
    pub out: Option<PathBuf>,

    /// Skip formatting the emitted Python.
    #[arg(long)]
    pub no_format: bool,
}

pub fn run(_args: BuildArgs) -> Result<()> {
    // Phase 0 stub: full pipeline will be wired in Phase 1–3.
    eprintln!("ttc build: pipeline not yet implemented (Phase 1+)");
    Ok(())
}
