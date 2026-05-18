//! `tyc fmt` — Typhon source formatter.
//!
//! Pipeline for a single `.ty` file:
//!
//! 1. Pre-process: rewrite Typhon-specific sugar (`model:`, `interface:`,
//!    `unsafe:`, `comptime`, `gather:`, `go`, `lazy`, `?` nullability) so
//!    the underlying parser sees plain Python. `let` / `mut` are left in
//!    place — the vendored Ruff parser recognises them natively.
//! 2. Parse: verify the source is syntactically valid via the vendored
//!    Ruff parser (`tyc_syntax::parse_module`).
//! 3. Normalise: apply lightweight whitespace normalisation to the
//!    pre-processed source (trailing spaces, final newline). Comments and
//!    blank lines are preserved.
//! 4. Post-process: restore the keywords that *were* stripped (model /
//!    impl / extend / interface / unsafe / comptime / lazy / gather / go).
//!
//! Full AST-based reformatting (which would drop comments) is deferred to
//! a later phase when a comment-preserving CST emitter is available.

use std::path::Path;

use tyc_diagnostics::TycError;
use tyc_syntax::{
    parse_module,
    preprocess::{
        expand_gather_blocks, expand_go_calls, expand_pipes, expand_question_ops,
        expand_with_chains, postprocess_full, preprocess,
    },
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
#[allow(clippy::result_large_err)]
pub fn format_source(source: &str, path: &str) -> Result<FormatResult, TycError> {
    // Step 1: pre-process — strip Typhon keywords.
    let prep = preprocess(source);

    // Step 2: parse — validate syntax, discard AST.
    //
    // The formatter does not want to rewrite the user's `?`, `|>`, or
    // `with`-chain syntax (that's `tyc build`'s job), but the underlying
    // Python parser cannot accept those constructs directly. Expand them in
    // a throw-away copy of the source purely for validation; the normalised
    // output below is still derived from `prep.python_source` so the Typhon
    // sugar is preserved when the file is rewritten.
    let validation_input = expand_question_ops(&expand_pipes(&expand_with_chains(
        &expand_go_calls(&expand_gather_blocks(&prep.python_source)),
    )));
    parse_module(&validation_input).map_err(|e| {
        let offset = usize::from(e.location.start());
        TycError::parse(path, &validation_input, e.to_string(), offset)
    })?;

    // Step 3: normalise whitespace on the pre-processed source.
    //
    // Phase 0 normalisation rules:
    //   • Strip trailing whitespace from each line.
    //   • Ensure the file ends with exactly one newline.
    //   • Expand tabs to 4 spaces.
    let normalised = normalise_whitespace(&prep.python_source);

    // Step 4: post-process — restore let/mut keywords, `?` sugar, and lazy imports.
    let output = postprocess_full(
        &normalised,
        &prep.stripped,
        &prep.optionals,
        &prep.lazy_imports,
    );

    let changed = output != source;
    Ok(FormatResult { output, changed })
}

/// Normalise whitespace in a Python-compatible source string.
///
/// - Expands tabs found in leading indentation to 4 spaces. Tabs after the
///   first non-whitespace character (e.g. inside string literals) are left
///   alone so their contents aren't corrupted.
/// - Strips trailing whitespace from each line, including reducing
///   whitespace-only lines to blank lines.
/// - Ensures the file ends with exactly one `\n`.
fn normalise_whitespace(source: &str) -> String {
    if source.is_empty() {
        return String::new();
    }
    let mut result = String::with_capacity(source.len());
    for raw_line in source.lines() {
        // Trim trailing whitespace first so whitespace-only lines collapse
        // to empty before we look for the indent boundary.
        let line = raw_line.trim_end();
        let indent_end = line
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(line.len());
        let leading = &line[..indent_end];
        let rest = &line[indent_end..];
        for ch in leading.chars() {
            if ch == '\t' {
                result.push_str("    ");
            } else {
                result.push(ch);
            }
        }
        result.push_str(rest);
        result.push('\n');
    }
    result
}

/// Format the `.ty` file at `path` in place.
///
/// Returns `true` if the file was changed.
#[allow(clippy::result_large_err)]
pub fn format_file(path: &Path) -> Result<bool, TycError> {
    let source =
        std::fs::read_to_string(path).map_err(|e| TycError::io(path.display().to_string(), &e))?;

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
    fn format_let_declaration() {
        let src = "let x: int = 1\n";
        let result = format_source(src, "<test>").unwrap();
        // The let keyword must be preserved through the format round-trip
        // (the soft-keyword survives ruff's Python parser via preprocessing).
        assert!(
            result.output.contains("let x"),
            "output should contain 'let x', got: {}",
            result.output
        );
    }

    #[test]
    fn format_mut_declaration() {
        let src = "mut count: int = 0\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("mut count"),
            "output should contain 'mut count', got: {}",
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
        let src = "# a comment\nlet x: int = 1\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("# a comment"),
            "comments must be preserved"
        );
        assert!(result.output.contains("let x"));
    }

    #[test]
    fn format_error_on_invalid_syntax() {
        let result = format_source("def (broken:", "<test>");
        assert!(result.is_err());
    }

    #[test]
    fn format_expands_leading_tabs() {
        let src = "def f():\n\tx: int = 1\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("    x: int = 1"),
            "leading tab should expand to spaces, got: {:?}",
            result.output
        );
    }

    #[test]
    fn format_trims_whitespace_only_lines() {
        // A blank line containing only spaces must be reduced to an empty
        // line, matching the pre-refactor behaviour. (The leading-indent-only
        // tab-expansion path could otherwise leave the original whitespace
        // verbatim on whitespace-only lines.)
        let src = "x: int = 1\n   \nlet y: int = 2\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("\n\nlet y"),
            "whitespace-only line must collapse to empty, got: {:?}",
            result.output
        );
    }

    #[test]
    fn format_accepts_question_operator() {
        // `tyc fmt` must not reject Typhon-only syntax that the underlying
        // Python parser would otherwise refuse. The source is preserved verbatim.
        let src = "\
def run() -> Result[int, str]:
    let x = load()?
    return Ok(x)
";
        let result = format_source(src, "<test>").unwrap();
        assert!(result.output.contains("load()?"), "got:\n{}", result.output);
        assert!(result.output.contains("let x"), "got:\n{}", result.output);
    }

    #[test]
    fn format_accepts_pipe_operator() {
        let src = "y = x |> f |> g\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(result.output.contains("|>"), "got:\n{}", result.output);
    }

    #[test]
    fn format_accepts_with_chain() {
        let src = "\
def run() -> Result[int, str]:
    with x = f()?:
        return Ok(x)
    else err:
        return Err(err)
";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("with x = f()?:"),
            "got:\n{}",
            result.output
        );
        assert!(
            result.output.contains("else err:"),
            "got:\n{}",
            result.output
        );
    }

    #[test]
    fn format_accepts_lazy_import() {
        let src = "lazy import np = numpy\n\nx = np.array([1, 2, 3])\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("lazy import np = numpy"),
            "lazy import must be preserved by formatter, got:\n{}",
            result.output
        );
    }

    #[test]
    fn format_preserves_tab_escape_in_string() {
        // `\t` inside a string literal is a two-character escape (backslash +
        // 't') in the source; it must survive normalisation intact.
        let src = "x = \"hello\\tworld\"\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("\\t"),
            "string \\t escape must be preserved, got: {:?}",
            result.output
        );
    }
}
