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
    after_help = concat!(
        "Documentation:        https://typhon.dev\n",
        "Language reference:   https://typhon.dev/lang\n",
        "Migrate Python:       tyc migrate <path.py>\n",
        "Cheat sheet:          tyc cheatsheet\n",
        "Diagnostic catalog:   tyc explain <code>\n",
        "Editor integration:   tyc lsp",
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
    Ty(commands::ty::TyArgs),

    /// Probe emitted `.pyi` stubs against the running module via mypy's `stubtest`.
    Stubtest(commands::stubtest::StubtestArgs),

    /// Launch an interactive Typhon REPL.
    Repl(commands::repl::ReplArgs),

    /// Add a Python package to the project.
    Add(commands::deps::AddArgs),

    /// Remove a Python package from `typhon.toml` and re-sync.
    Remove(commands::deps::RemoveArgs),

    /// Materialise `[dependencies]` into a generated `pyproject.toml`.
    Sync(commands::deps::SyncArgs),

    /// Build the project and launch the emitted Python under a debugger.
    Debug(commands::debug::DebugArgs),

    /// Build the project and execute the emitted Python in one step.
    Run(commands::run::RunArgs),

    /// Print the catalog entry for a diagnostic code (mirrors `rustc --explain`).
    Explain(commands::explain::ExplainArgs),

    /// Print the 30-second Typhon cheat sheet to stdout.
    Cheatsheet(commands::cheatsheet::CheatsheetArgs),
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
        Commands::Stubtest(args) => commands::stubtest::run(args),
        Commands::Repl(args) => commands::repl::run(args),
        Commands::Debug(args) => commands::debug::run(args),
        Commands::Run(args) => commands::run::run(args),
        Commands::Add(args) => commands::deps::run_add(args),
        Commands::Remove(args) => commands::deps::run_remove(args),
        Commands::Sync(args) => commands::deps::run_sync(args),
        Commands::Explain(args) => commands::explain::run(args),
        Commands::Cheatsheet(args) => commands::cheatsheet::run(args),
    }
}
