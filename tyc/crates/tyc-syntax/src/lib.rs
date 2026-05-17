//! Typhon syntax — lexer, parser, and AST extensions.
//!
//! Two parser back-ends are exposed:
//!
//! * The legacy [`parser`] module wraps `rustpython_parser`. Consumer crates
//!   that haven't been ported yet still use this path; the `val` / `var`
//!   keywords are stripped by the preprocessor before the source is handed
//!   off, and restored at codegen time.
//! * The new [`ruff`] module wraps the vendored fork of `ruff_python_parser`
//!   in [`vendor/ruff_python_parser`](../../vendor/ruff_python_parser).
//!   This back-end recognises `val` / `var` as first-class soft keywords;
//!   the resulting `StmtAssign` / `StmtAnnAssign` AST node carries a
//!   `mutability: Option<Mutability>` field directly. Consumer crates are
//!   being migrated to this back-end incrementally — see
//!   [`vendor/README.md`](../../vendor/README.md) for the plan.

pub mod lexer;
pub mod parser;
pub mod preprocess;
pub mod ruff;

pub use rustpython_ast as ast;
pub use rustpython_parser::{Mode, Parse};
