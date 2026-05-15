//! `ttc profile` — instrument emitted code for hot-function detection.

use std::path::PathBuf;

use clap::Args;
use miette::Result;

/// Arguments for `ttc profile`.
#[derive(Args, Debug)]
pub struct ProfileArgs {
    /// Project directory.
    #[arg(value_name = "PATH", default_value = ".")]
    pub path: PathBuf,
}

pub fn run(_args: ProfileArgs) -> Result<()> {
    // Phase 4 stub.
    eprintln!("ttc profile: profiling instrumentation not yet implemented (Phase 4+)");
    Ok(())
}
