//! Regression tests for the 2026-10-03 review's W7 workstream (preprocessor,
//! `tyc fmt`, lowering). Kept in their own integration-test file so they do
//! not collide with concurrent edits to `preprocess.rs`'s inline test module.

use tyc_syntax::preprocess::{postprocess_full, StrippedOptional};

/// W7-01: a stale `?` column must never index a line inside a multi-byte
/// character. `postprocess_full` used to slice `line[col..]` unchecked and
/// panicked with "byte index N is not a char boundary".
#[test]
fn postprocess_skips_a_stale_optional_column_inside_a_multibyte_char() {
    let normalised = "def f(a: int, b: Üü | None) -> int:\n";
    // Column 18 is inside `Ü` (bytes 17..19).
    let optionals = [StrippedOptional {
        line_index: 0,
        python_col: 18,
    }];
    let out = postprocess_full(normalised, &[], &optionals, &[]);
    assert_eq!(out, normalised, "a stale column is skipped, not applied");
}

/// The unshifted case still restores.
#[test]
fn postprocess_restores_an_exact_optional_column() {
    let normalised = "x: int | None = None\n";
    let optionals = [StrippedOptional {
        line_index: 0,
        python_col: 6,
    }];
    let out = postprocess_full(normalised, &[], &optionals, &[]);
    assert_eq!(out, "x: int? = None\n");
}
