//! Typhon parser — thin wrapper around `rustpython_parser`.
//!
//! Phase 0: delegates entirely to the fallback `rustpython_parser` crate.
//! Once `vendor/ruff_python_parser` is in place this module will switch to
//! it without callers needing to change.

use rustpython_parser::{parse, Mode};
use rustpython_ast::Mod;

pub use rustpython_parser::ParseError;

/// Parse a Typhon source file.
///
/// `source` should be the *pre-processed* Python-compatible source returned by
/// [`crate::preprocess::preprocess`].  `path` is used only for error messages.
pub fn parse_module(source: &str, path: &str) -> Result<Mod, ParseError> {
    parse(source, Mode::Module, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_assignment() {
        let ast = parse_module("x: int = 1\n", "<test>").unwrap();
        assert!(matches!(ast, rustpython_ast::Mod::Module(_)));
    }

    #[test]
    fn parse_error_on_invalid() {
        assert!(parse_module("def (broken:", "<test>").is_err());
    }
}
