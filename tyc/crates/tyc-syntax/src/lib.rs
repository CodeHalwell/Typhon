//! Typhon syntax — lexer, parser, and AST extensions.
//!
//! The canonical AST is the vendored fork of `ruff_python_ast` in
//! [`vendor/ruff_python_ast`](../../vendor/ruff_python_ast). The parser is
//! the vendored fork of `ruff_python_parser`, which recognises `let` and
//! `mut` as first-class soft keywords — the resulting `StmtAssign` and
//! `StmtAnnAssign` AST nodes carry a `mutability: Option<Mutability>`
//! field directly, so no preprocessor pass is required for the binding
//! prefixes.
//!
//! The [`preprocess`] module still rewrites surface sugar (`?`
//! nullability, `model:`, `interface:`, `unsafe:`, `comptime`,
//! `gather:`, `go`, `lazy`, `with`-chains, the `?` error-propagation
//! operator) into plain Python before parsing.

pub mod lexer;
pub mod parser;
pub mod preprocess;
pub mod ruff;

pub use ruff::{ParseError, Parsed, parse_module};
pub use ruff_python_ast as ast;
