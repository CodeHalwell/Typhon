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

// ── W7-02: string state across a backslash-continued line ────────────────────

use tyc_syntax::lexmask::{scan_line_kinds, ByteKind, LexMask};
use tyc_syntax::preprocess::{expand_sugar, preprocess};

/// Build the Python the check/build pipeline parses.
fn lower(src: &str) -> String {
    preprocess(&expand_sugar(src, true)).python_source
}

/// Whether the lowered Python parses.
fn parses(src: &str) -> bool {
    tyc_syntax::parse_module(src).is_ok()
}

#[test]
fn lexmask_carries_a_backslash_continued_double_quoted_string() {
    // A backslash before the newline continues a single-line string literal
    // onto the next physical line; the continuation is string text.
    let src = "b = \"a,b\\\n    c,d  e#f\"\nprint(b)\n";
    let mask = LexMask::new(src);
    assert!(
        mask.line_starts_in_string(1),
        "line 2 starts inside the string"
    );
    assert!(
        !mask.line_starts_in_string(2),
        "the string closed on line 2"
    );
    // `#f` on the continuation is string text, not a comment, so the code
    // portion of line 2 runs to the end of the line.
    let line2 = "    c,d  e#f\"\n";
    assert_eq!(mask.line_code_end(1), line2.len() - 1);
}

#[test]
fn lexmask_carries_a_backslash_continued_single_quoted_string() {
    let src = "b = 'x?\\\ny as! z'\nq = 1\n";
    let mask = LexMask::new(src);
    assert!(mask.line_starts_in_string(1));
    assert!(!mask.line_starts_in_string(2));
}

#[test]
fn lexmask_escaped_backslash_at_end_of_line_is_not_a_continuation() {
    // `"a\\"` ends with an *escaped backslash*: the string is closed.
    let src = "b = \"a\\\\\"\nq = 1\n";
    let mask = LexMask::new(src);
    assert!(!mask.line_starts_in_string(1));
}

#[test]
fn scan_line_kinds_threads_a_continued_string_without_terminators() {
    // Callers that thread `in_string` over `str::lines()` (no terminator)
    // must see the same carry.
    let mut state = None;
    let _ = scan_line_kinds("b = \"a,b\\", &mut state);
    assert!(state.is_some(), "the open string is carried");
    let kinds = scan_line_kinds("    c,d  e#f\"", &mut state);
    assert!(state.is_none(), "the string closed");
    assert!(kinds.iter().all(|k| *k == ByteKind::StringText));
}

#[test]
fn sugar_inside_a_backslash_continued_string_is_left_alone() {
    // `?`, `as!` and `rescue` on the continuation line of a single-quoted
    // string were rewritten as code: `tyc::parse` on valid source, or a
    // spurious module-level-rescue error.
    for src in [
        "def f() -> str:\n    let s: str = \"what\\\n x? y\"\n    return s\n",
        "def f() -> str:\n    let s: str = \"what\\\n x as! int y\"\n    return s\n",
        "def f() -> str:\n    let s: str = \"what\\\n rescue e: oops\"\n    return s\n",
        "S: str = 'a\\\nrescue e: b'\n",
    ] {
        let out = lower(src);
        assert!(
            parses(&out),
            "lowered source must parse:\n{src}\n---\n{out}"
        );
        assert!(
            !out.contains("__typhon") && !out.contains("try_result"),
            "string content was rewritten as code:\n{out}"
        );
    }
}
