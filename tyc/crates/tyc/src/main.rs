//! `tyc` — Typhon compiler and language server.
//!
//! A single Rust binary with clap subcommands covering all compiler
//! operations: build, check, format, LSP, init, trace, and profile.

mod cli;
mod commands;
mod config;

use miette::Result;

fn main() -> Result<()> {
    cli::run()
}
