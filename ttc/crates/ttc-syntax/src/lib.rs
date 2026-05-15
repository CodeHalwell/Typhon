//! Typhon syntax — lexer, parser, and AST extensions.
//!
//! Phase 0: wraps `rustpython_parser` (crates.io fallback) and adds the two
//! Typhon-specific keyword tokens `val` and `var`.  Later phases will fork
//! `ruff_python_parser` into `vendor/` and extend it directly.

pub mod lexer;
pub mod parser;
pub mod preprocess;

pub use rustpython_ast as ast;
pub use rustpython_parser::{Mode, Parse};
