//! `tyc fmt` — Typhon source formatter.
//!
//! Pipeline for a single `.ty` file:
//!
//! 1. Pre-process: strip `val`/`var` keywords so the Python parser can handle
//!    the remainder of the grammar.
//! 2. Parse: verify the source is syntactically valid using
//!    `rustpython_parser`.
//! 3. Normalise: apply lightweight whitespace normalisation to the
//!    pre-processed source (trailing spaces, final newline).  Comments and
//!    blank lines are preserved.
//! 4. Post-process: restore `val`/`var` at the appropriate locations.
//!
//! Full AST-based reformatting (which would drop comments) is deferred to a
//! later phase when a comment-preserving CST emitter is available.

use std::path::Path;

use tyc_diagnostics::TycError;
use tyc_syntax::{
    parser::parse_module,
    preprocess::{postprocess, preprocess},
};

/// The outcome of formatting a single file.
#[derive(Debug)]
pub struct FormatResult {
    /// The formatted Typhon source.
    pub output: String,
    /// True if the content changed.
    pub changed: bool,
}

/// Format the Typhon source in `source`, returning the formatted string.
///
/// `path` is used only for diagnostic messages.
pub fn format_source(source: &str, path: &str) -> Result<FormatResult, TycError> {
    // Step 1: pre-process — strip Typhon keywords.
    let prep = preprocess(source);

    // Step 2: parse — validate syntax, discard AST.
    // Use `prep.python_source` as the diagnostic source text so that the
    // reported byte offset (which comes from parsing the preprocessed source)
    // aligns correctly with the displayed code.
    parse_module(&prep.python_source, path).map_err(|e| {
        let offset = usize::from(e.offset);
        TycError::parse(path, &prep.python_source, e.to_string(), offset)
    })?;

    // Step 3: normalise whitespace on the pre-processed source.
    //
    // Phase 0 normalisation rules:
    //   • Strip trailing whitespace from each line.
    //   • Ensure the file ends with exactly one newline.
    //   • Expand tabs to 4 spaces.
    let normalised = normalise_whitespace(&prep.python_source);

    // Step 4: post-process — restore val/var keywords and `?` sugar.
    let output = postprocess(&normalised, &prep.stripped, &prep.optionals);

    let changed = output != source;
    Ok(FormatResult { output, changed })
}

/// Normalise whitespace in a Python-compatible source string.
///
/// - Expands tabs to 4 spaces.
/// - Strips trailing whitespace from each line.
/// - Ensures the file ends with exactly one `\n`.
fn normalise_whitespace(source: &str) -> String {
    let expanded = source.replace('\t', "    ");
    let mut result = String::with_capacity(expanded.len());
    for line in expanded.lines() {
        result.push_str(line.trim_end());
        result.push('\n');
    }
    // If source was empty do not add a spurious newline.
    if source.is_empty() {
        return String::new();
    }
    result
}

/// Format the `.ty` file at `path` in place.
///
/// Returns `true` if the file was changed.
pub fn format_file(path: &Path) -> Result<bool, TycError> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| TycError::io(path.display().to_string(), &e))?;

    let path_str = path.display().to_string();
    let result = format_source(&source, &path_str)?;

    if result.changed {
        std::fs::write(path, result.output.as_bytes())
            .map_err(|e| TycError::io(path.display().to_string(), &e))?;
    }

    Ok(result.changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_plain_python() {
        let src = "x: int = 1\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(result.output.contains("x: int = 1"));
    }

    #[test]
    fn format_val_declaration() {
        let src = "val x: int = 1\n";
        let result = format_source(src, "<test>").unwrap();
        // The val keyword must be present in the output.
        assert!(
            result.output.contains("val x"),
            "output should contain 'val x', got: {}",
            result.output
        );
    }

    #[test]
    fn format_var_declaration() {
        let src = "var count: int = 0\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("var count"),
            "output should contain 'var count', got: {}",
            result.output
        );
    }

    #[test]
    fn format_strips_trailing_whitespace() {
        let src = "x: int = 1   \n";
        let result = format_source(src, "<test>").unwrap();
        assert_eq!(result.output, "x: int = 1\n");
        assert!(result.changed);
    }

    #[test]
    fn format_preserves_comments() {
        let src = "# a comment\nval x: int = 1\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("# a comment"),
            "comments must be preserved"
        );
        assert!(result.output.contains("val x"));
    }

    #[test]
    fn format_error_on_invalid_syntax() {
        let result = format_source("def (broken:", "<test>");
        assert!(result.is_err());
    }
}
