//! Ruff parser back-end — the migration target for Typhon's consumer crates.
//!
//! This module is the entry point to the Typhon fork of `ruff_python_parser`
//! that lives under [`vendor/ruff_python_parser`](../../../vendor/ruff_python_parser).
//! It exposes a small surface deliberately:
//!
//! * [`parse_module`] — parse Typhon source directly (no preprocessor needed
//!   for `let` / `mut`; sugar passes like `?`, `|>`, `with`-chains, and
//!   `gather:` still require preprocessing because they are surface syntax
//!   we don't extend the parser for).
//! * Re-exports of [`ast`] and the upstream [`Mutability`] enum so callers
//!   can pattern-match without taking a direct dep on the vendored crate.
//!
//! ## When to use which back-end
//!
//! New code should prefer [`parse_module`] in this module. Existing crates
//! still using [`crate::parser::parse_module`] will be ported one at a time
//! per the plan in `vendor/README.md`; until then the two back-ends coexist
//! and produce independent ASTs.

pub use ruff_python_ast as ast;
pub use ruff_python_ast::Mutability;
pub use ruff_python_parser::{ParseError, Parsed};

use ruff_python_ast::ModModule;

/// Parse a Typhon source file with the vendored Ruff parser.
///
/// `source` should contain Typhon source. `let` / `mut` are recognised as
/// soft keywords directly — no preprocessing pass is required for them.
/// Other Typhon-specific sugar (`?`, `|>`, `gather:`, `with`-chains, `go`,
/// etc.) is *not* yet known to this parser; callers that need to accept
/// that sugar should still preprocess it with the helpers in
/// [`crate::preprocess`] before calling this function.
pub fn parse_module(source: &str) -> Result<Parsed<ModModule>, ParseError> {
    ruff_python_parser::parse_module(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ast::Stmt;

    #[test]
    fn let_binding_carries_mutability() {
        let parsed = parse_module("let x: int = 1\n").expect("parse");
        let Stmt::AnnAssign(a) = parsed.into_syntax().body.into_iter().next().unwrap() else {
            panic!("expected StmtAnnAssign");
        };
        assert_eq!(a.mutability, Some(Mutability::Let));
    }

    #[test]
    fn mut_binding_carries_mutability() {
        let parsed = parse_module("mut counter = 0\n").expect("parse");
        let Stmt::Assign(a) = parsed.into_syntax().body.into_iter().next().unwrap() else {
            panic!("expected StmtAssign");
        };
        assert_eq!(a.mutability, Some(Mutability::Mut));
    }

    #[test]
    fn plain_python_still_parses() {
        let parsed = parse_module("def f(x: int) -> int:\n    return x * 2\n").expect("parse");
        assert_eq!(parsed.into_syntax().body.len(), 1);
    }

    #[test]
    fn let_identifier_outside_statement_start() {
        // `let` mid-expression is still a regular identifier.
        let parsed = parse_module("y = let + 1\n").expect("parse");
        let Stmt::Assign(a) = parsed.into_syntax().body.into_iter().next().unwrap() else {
            panic!("expected StmtAssign");
        };
        assert_eq!(a.mutability, None);
    }
}
