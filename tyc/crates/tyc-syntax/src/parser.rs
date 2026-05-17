//! Legacy `rustpython_parser` wrapper, kept for the duration of the
//! migration to the vendored Ruff parser. Will be removed in Step 9 of
//! the migration plan (see `vendor/README.md`).

use rustpython_ast::Mod;
use rustpython_parser::{Mode, parse};

pub use rustpython_parser::ParseError;

/// Parse a Typhon source file with the legacy rustpython back-end.
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
