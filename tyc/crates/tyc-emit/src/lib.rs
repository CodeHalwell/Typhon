//! Python code emitter.
//!
//! Converts a `ruff_python_ast::ModModule` back into Python source text.
//! In Phase 0 this is a hand-written pretty-printer that covers the Python
//! subset needed for round-trip testing.  Later phases will switch to a
//! vendored `ruff_python_codegen` + `ruff_python_formatter` pipeline.

mod printer;
mod stub;
mod stubtest;

pub use printer::Emitter;
pub use stub::emit_stub;
pub use stubtest::{compare_modules, StubTestFinding, StubTestKind};

use ruff_python_ast::ModModule;

/// Emit a [`ModModule`] AST node as Python source text.
pub fn emit(module: &ModModule) -> String {
    let mut emitter = Emitter::new();
    emitter.emit_mod(module);
    emitter.finish()
}

/// Emit a [`ModModule`] AST node and return a `(source, line_offsets)` pair.
///
/// `line_offsets[i]` is the byte offset in the preprocessed Typhon source
/// that was active when output line `i` (0-indexed) was emitted.  Callers
/// convert these offsets to line numbers using the preprocessed source text
/// to build the `lines` array stored in `.py.map` v2 files.
///
/// Synthesised statements (e.g. `import dataclasses` injected by the
/// desugar pass) carry a zero-length `TextRange`; they inherit the offset
/// of the nearest preceding real statement.
pub fn emit_with_line_offsets(module: &ModModule) -> (String, Vec<usize>) {
    let mut emitter = Emitter::new();
    emitter.emit_mod(module);
    let offsets = emitter.line_offsets.clone();
    (emitter.finish(), offsets)
}
