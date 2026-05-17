//! Vendored copy of `ruff_python_ast` — **scaffold only**.
//!
//! This crate is part of the Phase-0 follow-up to migrate Typhon off
//! `rustpython-parser` and onto an internal fork of the Ruff parser/AST
//! pair. See `vendor/README.md` for the migration plan.
//!
//! Today the crate is intentionally empty: the workspace compiles it
//! and the dependency edge `vendor/ruff_python_parser → ruff_python_ast`
//! is wired up so adding real source becomes a drop-in replacement,
//! but no consumer crate depends on this yet. All Typhon code still
//! routes through `rustpython-parser` 0.4 from crates.io.

#![allow(dead_code)]
#![allow(unused_imports)]

/// Marker indicating this is the scaffold build of the vendored Ruff AST.
/// Consumers can assert on this string in tests during the migration to
/// detect a half-finished swap.
pub const SCAFFOLD_MARKER: &str = "ruff_python_ast: scaffold";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffold_marker_is_present() {
        assert!(SCAFFOLD_MARKER.contains("scaffold"));
    }
}
