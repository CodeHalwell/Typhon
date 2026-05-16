//! Python code emitter.
//!
//! Converts a `rustpython_ast::Mod` back into Python source text.  In
//! Phase 0 this is a hand-written pretty-printer that covers the Python
//! subset needed for round-trip testing.  Later phases will switch to a
//! vendored `ruff_python_codegen` + `ruff_python_formatter` pipeline.

mod printer;
mod stub;

pub use printer::Emitter;
pub use stub::emit_stub;

use rustpython_ast::Mod;

/// Emit a [`Mod`] AST node as Python source text.
pub fn emit(module: &Mod) -> String {
    let mut emitter = Emitter::new();
    emitter.emit_mod(module);
    emitter.finish()
}
