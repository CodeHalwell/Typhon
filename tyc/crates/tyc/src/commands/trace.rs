//! `tyc trace` — map a Python traceback back to Typhon source.

use std::path::PathBuf;

use clap::Args;
use miette::Result;

/// Arguments for `tyc trace`.
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
    // Source-map generation (`.py.map` files) is not yet implemented; it will
    // be wired in once the emitter tracks Typhon→Python byte offset mappings.
    eprintln!("tyc trace: source-map tracing not yet implemented");
    Ok(())
}
