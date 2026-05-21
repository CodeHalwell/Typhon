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
///
/// Preserves Typhon's `let`/`mut` soft keywords so the output round-trips
/// through `tyc fmt`.  For valid-Python output (e.g. `tyc build`), use
/// [`emit_python`] or [`emit_python_with_line_offsets`].
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
///
/// Preserves `let`/`mut` like [`emit`].  Use
/// [`emit_python_with_line_offsets`] for build output that must run under
/// CPython.
pub fn emit_with_line_offsets(module: &ModModule) -> (String, Vec<usize>) {
    let mut emitter = Emitter::new();
    emitter.emit_mod(module);
    let offsets = emitter.line_offsets.clone();
    (emitter.finish(), offsets)
}

/// Like [`emit`] but suppresses Typhon's `let`/`mut` soft keywords so the
/// output is valid Python.
pub fn emit_python(module: &ModModule) -> String {
    let mut emitter = Emitter::new();
    emitter.suppress_mutability_keywords();
    emitter.emit_mod(module);
    emitter.finish()
}

/// Like [`emit_with_line_offsets`] but suppresses Typhon's `let`/`mut`
/// soft keywords so the output is valid Python.  This is the path
/// `tyc build` uses; the AST-level suppression is safe to apply to all
/// statements (including those nested inside string literals — which the
/// printer never visits) and avoids the brittleness of a post-pass
/// text rewrite.
pub fn emit_python_with_line_offsets(module: &ModModule) -> (String, Vec<usize>) {
    emit_python_with_line_offsets_for_target(module, 0)
}

/// Like [`emit_python_with_line_offsets`] but with an explicit target
/// Python minor version (`3.X`). When `< 12`, PEP 695 syntax in the
/// AST is lowered to the legacy `TypeVar` / `Generic[T]` /
/// `X: TypeAlias = Y` shapes so the emitted module parses on older
/// interpreters (FINDINGS #47). `0` disables lowering (the previous
/// default behaviour).
pub fn emit_python_with_line_offsets_for_target(
    module: &ModModule,
    target_minor: u8,
) -> (String, Vec<usize>) {
    emit_python_with_source_for_target(module, target_minor, None)
}

/// Like [`emit_python_with_line_offsets_for_target`], but additionally
/// accepts the original (preprocessed) source so syntactic choices
/// the AST collapses can be round-tripped. Currently used to recover
/// the user's bracket style on sequence patterns (`[a, b]` vs
/// `(a, b)`) — O27 / FINDINGS #111. Pass `None` to keep the previous
/// default behaviour.
///
/// The `source` is held behind an `Arc<str>` inside the printer; a
/// `&str` is converted via `Arc::from`, which is a single allocation
/// the caller can amortise by sharing the `Arc` across passes (e.g.
/// the build pipeline already holds the preprocessed text — clone
/// the `Arc` rather than pass a `&str` here to skip the copy
/// entirely). Copilot review on PR #96.
pub fn emit_python_with_source_for_target(
    module: &ModModule,
    target_minor: u8,
    source: Option<&str>,
) -> (String, Vec<usize>) {
    let mut emitter = Emitter::new();
    emitter.suppress_mutability_keywords();
    emitter.set_python_target(target_minor);
    if let Some(s) = source {
        emitter.set_source(std::sync::Arc::from(s));
    }
    emitter.emit_mod(module);
    let offsets = emitter.line_offsets.clone();
    (emitter.finish(), offsets)
}
