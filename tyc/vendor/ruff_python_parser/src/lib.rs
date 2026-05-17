//! Vendored copy of `ruff_python_parser` — **scaffold only**.
//!
//! This crate is part of the Phase-0 follow-up to migrate Typhon off
//! `rustpython-parser` and onto an internal fork of the Ruff parser/AST
//! pair. See `vendor/README.md` for the migration plan.
//!
//! Today the crate is intentionally empty. The dependency edge on
//! `ruff_python_ast` is wired up so swapping in real source from the
//! upstream Ruff repository is a drop-in replacement that does not
//! touch the workspace topology.

#![allow(dead_code)]
#![allow(unused_imports)]

pub use ruff_python_ast;

/// Marker indicating this is the scaffold build of the vendored Ruff
/// parser. Consumers can assert on this string in tests during the
/// migration to detect a half-finished swap.
pub const SCAFFOLD_MARKER: &str = "ruff_python_parser: scaffold";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffold_marker_is_present() {
        assert!(SCAFFOLD_MARKER.contains("scaffold"));
    }

    #[test]
    fn ast_re_export_works() {
        assert_eq!(
            ruff_python_ast::SCAFFOLD_MARKER,
            "ruff_python_ast: scaffold"
        );
    }
}
