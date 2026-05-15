//! `ttc trace` — map a Python traceback back to Typhon source.

use std::path::PathBuf;

use clap::Args;
use miette::Result;

/// Arguments for `ttc trace`.
#[derive(Args, Debug)]
pub struct TraceArgs {
    /// Python traceback file to map (reads from stdin if omitted).
    #[arg(value_name = "TRACEBACK")]
    pub traceback: Option<PathBuf>,

    /// Directory containing `.py.map` source-map files.
    #[arg(long, value_name = "DIR")]
    pub map_dir: Option<PathBuf>,
}

pub fn run(_args: TraceArgs) -> Result<()> {
    // Phase 3 stub: source-map reading will be implemented alongside the emitter.
    eprintln!("ttc trace: source-map tracing not yet implemented (Phase 3+)");
    Ok(())
}
