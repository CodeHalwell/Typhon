//! CLI definition using clap v4 derive macros.

use clap::{Parser, Subcommand};
use miette::Result;

use crate::commands;

/// tyc — the Typhon compiler and language-server binary.
#[derive(Parser, Debug)]
#[command(
    name = "tyc",
    about = "The Typhon compiler and language server",
    long_about = concat!(
        "tyc is the single binary for the Typhon language toolchain.\n\n",
        "Typhon is a statically-typed, stricter superset of Python that\n",
        "compiles to clean, readable CPython 3.13+ code with no runtime\n",
        "dependency on the toolchain."
    ),
    version,
    propagate_version = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

/// All available tyc subcommands.
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Full pipeline: parse, check, analyse, desugar, emit, format.
    Build(commands::build::BuildArgs),

    /// Parse and type-check only — no code emission. For use in CI.
    Check(commands::check::CheckArgs),

    /// Format `.ty` source files in place.
    Fmt(commands::fmt::FmtArgs),

    /// Run as a Language Server (LSP) on stdio.
    Lsp(commands::lsp::LspArgs),

    /// Scaffold a new Typhon project.
    Init(commands::init::InitArgs),

    /// Map a Python traceback back to Typhon source via `.py.map` files.
    Trace(commands::trace::TraceArgs),

    /// Instrument emitted code for hot-function detection (opt-in).
    Profile(commands::profile::ProfileArgs),

    /// Convert typed Python (`.py`) into Typhon (`.ty`) using a set of
    /// conservative textual rewrites.
    Migrate(commands::migrate::MigrateArgs),

    /// Run Astral's `ty` against the emitted Python.
    ///
    /// Builds the project (into a temp directory by default) and invokes
    /// `ty check <out-dir>` as a subprocess. Requires `ty` to be installed
    /// separately (`pip install ty` or `uv tool install ty`).
    Ty(commands::ty::TyArgs),

    /// Launch an interactive Typhon REPL.
    ///
    /// Accumulates `.ty` source across prompts and pipes each evaluation
    /// through the full compile pipeline plus a Python subprocess.
    Repl(commands::repl::ReplArgs),

    /// Build the project and launch the emitted Python under a debugger.
    ///
    /// v1 thin wrapper that runs `python -m pdb build/main.py`. A full
    /// source-mapping Typhon-native debugger is a Phase-5 item.
    Debug(commands::debug::DebugArgs),
}

/// Entry point called from `main`.
pub fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Build(args) => commands::build::run(args),
        Commands::Check(args) => commands::check::run(args),
        Commands::Fmt(args) => commands::fmt::run(args),
        Commands::Lsp(args) => commands::lsp::run(args),
        Commands::Init(args) => commands::init::run(args),
        Commands::Trace(args) => commands::trace::run(args),
        Commands::Profile(args) => commands::profile::run(args),
        Commands::Migrate(args) => commands::migrate::run(args),
        Commands::Ty(args) => commands::ty::run(args),
        Commands::Repl(args) => commands::repl::run(args),
        Commands::Debug(args) => commands::debug::run(args),
    }
}
