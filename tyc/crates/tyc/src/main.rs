//! `tyc` — Typhon compiler and language server.
//!
//! A single Rust binary with clap subcommands covering all compiler
//! operations: build, check, format, LSP, init, trace, and profile.

mod cli;
mod commands;
mod config;

use miette::Result;

/// Stack reserved for the compiler worker thread. The recursive descent in the
/// parser, type checker, and VM can go deep on pathological input — a long flat
/// `a + b + c + …` chain or deeply nested brackets builds a deep AST that the
/// AST walkers recurse over. On the default ~8 MB main-thread stack such input
/// overflowed and aborted the process (SIGABRT) instead of producing a clean
/// diagnostic or result. Running the work on a generously-sized stack (lazily
/// committed, so it costs only address space) pushes the ceiling far past any
/// realistic program — mirroring how rustc/clippy run on a large worker stack.
const WORKER_STACK_SIZE: usize = 256 * 1024 * 1024;

fn main() -> Result<()> {
    let worker = std::thread::Builder::new()
        .name("tyc-main".to_owned())
        .stack_size(WORKER_STACK_SIZE)
        .spawn(cli::run)
        .expect("failed to spawn tyc worker thread");
    match worker.join() {
        Ok(result) => result,
        // The worker panicked; resume unwinding on the main thread so the
        // process still exits through the normal panic path and message.
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
