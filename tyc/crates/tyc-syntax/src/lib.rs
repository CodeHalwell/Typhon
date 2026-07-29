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
pub(crate) mod lexmask;
pub mod mro;
pub mod preprocess;
pub mod ruff;

pub use ruff::{parse_expression, parse_module, ParseError, Parsed};
pub use ruff_python_ast as ast;

#[cfg(test)]
mod tests {

    #[test]
    fn soft_keywords_are_valid_binding_names() {
        // A soft keyword is a valid identifier everywhere Python allows one,
        // and Typhon is a superset — `let match = re.match(...)` is ordinary
        // code (and the conventional name for a regex result). The parser
        // only accepted `TokenKind::Name` after `let` / `mut`, so `let` fell
        // through as an identifier and the line failed to parse.
        for src in [
            "let match = 1\n",
            "mut match = 1\n",
            "let type = 2\n",
            "let case = 3\n",
            "mut type: int = 4\n",
        ] {
            assert!(
                parse_module(src).is_ok(),
                "should parse as a binding: {src:?}"
            );
        }
    }
    use super::*;
}
