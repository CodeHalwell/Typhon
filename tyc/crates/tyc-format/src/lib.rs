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
        expand_gather_blocks, expand_go_calls, expand_multiline_guards, expand_pipes,
        expand_question_ops, expand_with_chains, postprocess_full, preprocess,
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
    let validation_input =
        expand_question_ops(&expand_pipes(&expand_with_chains(&expand_go_calls(
            &expand_gather_blocks(&expand_multiline_guards(&prep.python_source)),
        ))));
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
/// In addition to the historical tab / trailing-whitespace / final-newline
/// rules, this pass applies a small set of style normalisations chosen to
/// match PEP 8 without touching the contents of string literals:
///
/// - Collapse runs of three or more blank lines to two.
/// - Ensure a single space after a `#` token in line comments.
/// - Normalise `, ` after a comma (outside strings) to a single space.
///
/// The pass scans each logical line and skips edits within `'…'` / `"…"` /
/// triple-quoted regions so doc strings and embedded code stay verbatim.
fn normalise_whitespace(source: &str) -> String {
    if source.is_empty() {
        return String::new();
    }
    let mut result = String::with_capacity(source.len());
    let mut consecutive_blank = 0u32;
    let mut in_triple: Option<char> = None;
    for raw_line in source.lines() {
        // Triple-quoted block: keep raw, but still strip trailing spaces and
        // expand the leading tabs so indentation matches the file style.
        let (line_owned, exited_triple) = if let Some(q) = in_triple {
            let exited = line_closes_triple_quote(raw_line, q);
            let l = raw_line.trim_end().to_owned();
            (l, exited)
        } else {
            let trimmed = raw_line.trim_end();
            (apply_simple_style_rules(trimmed), false)
        };

        if in_triple.is_none() && !exited_triple {
            in_triple = detect_triple_quote_open(&line_owned);
        } else if exited_triple {
            in_triple = None;
        }

        let line = line_owned.as_str();
        let indent_end = line
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(line.len());
        let leading = &line[..indent_end];
        let rest = &line[indent_end..];

        if rest.is_empty() {
            consecutive_blank += 1;
            if consecutive_blank <= 2 {
                result.push('\n');
            }
            continue;
        }
        consecutive_blank = 0;

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

/// Apply spacing normalisations that touch ordinary code regions only.
///
/// The implementation walks the line once, tracking single- and double-
/// quoted string regions so the edits never reach into a literal.  It
/// rejects backslash-escaped quotes, raw strings (the prefix is invisible
/// at this point — quotes still bracket the literal), and f-strings (same
/// reasoning).
fn apply_simple_style_rules(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            out.push(c);
            if c == '\\' {
                if let Some(&n) = chars.peek() {
                    out.push(n);
                    chars.next();
                }
                continue;
            }
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                out.push(c);
            }
            '#' => {
                // Normalise `#foo` → `# foo`, but leave shebangs and
                // double-hash sectioning comments (`## …`, `#!…`) alone.
                out.push('#');
                let next = chars.peek().copied();
                match next {
                    None => {}
                    Some(n) if n == '!' || n == '#' || n == ' ' || n == '\t' => {}
                    Some(_) => out.push(' '),
                }
                // Append the rest of the line verbatim — comments cannot
                // contain strings/code so further edits don't apply.
                for c in chars.by_ref() {
                    out.push(c);
                }
            }
            ',' => {
                out.push(',');
                // Collapse multi-space `,    ` to `, ` but only when the
                // next character is whitespace; `,)` (last arg) stays as-is.
                let mut peek_iter = chars.clone();
                let mut saw_space = false;
                while let Some(&n) = peek_iter.peek() {
                    if n == ' ' || n == '\t' {
                        saw_space = true;
                        peek_iter.next();
                    } else {
                        break;
                    }
                }
                if saw_space {
                    // Consume the whitespace run from the real iterator
                    // and emit a single space.
                    while let Some(&n) = chars.peek() {
                        if n == ' ' || n == '\t' {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    out.push(' ');
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Returns the quote character that opens an *unterminated* triple-quoted
/// string starting on this line, if any.  When a triple-quoted string is
/// opened *and* closed on the same line we treat it as fully balanced and
/// return `None`.
///
/// Scanned character-by-character with awareness of regular single- and
/// double-quoted regions, so a sequence like `x = "'''"` (which contains
/// `'''` inside a normal string) does not falsely look like a
/// triple-quote opener.
fn detect_triple_quote_open(line: &str) -> Option<char> {
    let mut counts = [0u32, 0u32]; // [single, double]
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut inside: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = inside {
            // Inside a regular (non-triple) string. Skip escapes.
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == q {
                inside = None;
            }
            i += 1;
            continue;
        }
        if (b == b'"' || b == b'\'')
            && i + 2 < bytes.len()
            && bytes[i + 1] == b
            && bytes[i + 2] == b
        {
            let idx = if b == b'\'' { 0 } else { 1 };
            counts[idx] += 1;
            i += 3;
            continue;
        }
        if b == b'"' || b == b'\'' {
            inside = Some(b);
            i += 1;
            continue;
        }
        if b == b'#' {
            // Comment end — anything past `#` (outside a string) is text.
            break;
        }
        i += 1;
    }
    if !counts[0].is_multiple_of(2) {
        return Some('\'');
    }
    if !counts[1].is_multiple_of(2) {
        return Some('"');
    }
    None
}

/// Whether the remainder of a multi-line triple-quoted string is closed
/// on this `line` by the matching `q` triple.  The check is intentionally
/// loose (string match) because once we are *inside* a triple-quoted
/// region, regular-quote string syntax does not apply — the only way out
/// is the matching triple.
fn line_closes_triple_quote(line: &str, q: char) -> bool {
    let triple: String = std::iter::repeat_n(q, 3).collect();
    line.contains(&triple)
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
    fn format_collapses_excess_blank_lines() {
        let src = "x: int = 1\n\n\n\n\nlet y: int = 2\n";
        let result = format_source(src, "<test>").unwrap();
        // Three or more blank lines collapse to exactly two blank lines.
        assert!(
            result.output.contains("\n\n\nlet y"),
            "expected two blank lines between statements; got: {:?}",
            result.output
        );
        assert!(
            !result.output.contains("\n\n\n\nlet y"),
            "expected at most two blank lines; got: {:?}",
            result.output
        );
    }

    #[test]
    fn format_normalises_comment_spacing() {
        let src = "let x: int = 1  #no space\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("# no space"),
            "comment should gain a space after #; got: {:?}",
            result.output
        );
    }

    #[test]
    fn format_preserves_shebang_and_section_headers() {
        let src = "#!/usr/bin/env python\n## section header\nlet x: int = 1\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.starts_with("#!/usr/bin/env python\n"),
            "shebang must be preserved verbatim; got: {:?}",
            result.output
        );
        assert!(
            result.output.contains("## section header"),
            "## section headers must be preserved; got: {:?}",
            result.output
        );
    }

    #[test]
    fn format_normalises_comma_spacing() {
        let src = "let xs = (1,2,    3)\n";
        let result = format_source(src, "<test>").unwrap();
        // Inside parens the formatter doesn't fix tight commas, but a comma
        // already followed by space(s) collapses to exactly one space.
        assert!(
            result.output.contains("1,2, 3") || result.output.contains("1, 2, 3"),
            "comma whitespace should collapse to a single space; got: {:?}",
            result.output
        );
    }

    #[test]
    fn format_leaves_hash_inside_string_alone() {
        // A `#` inside a string is not a comment — it must stay verbatim.
        let src = "let s: str = \"#no-space\"\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("\"#no-space\""),
            "hash inside string must be preserved verbatim; got: {:?}",
            result.output
        );
    }

    #[test]
    fn format_preserves_triple_quoted_block_content() {
        let src = "x = \"\"\"\nhello,world\nblock\n\"\"\"\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("hello,world"),
            "triple-quoted contents must stay verbatim; got: {:?}",
            result.output
        );
    }

    #[test]
    fn format_treats_triple_inside_regular_string_as_text() {
        // `"'''"` is a regular string containing three apostrophes — it
        // must NOT count as opening a triple-quoted block, otherwise
        // subsequent lines stop receiving normalisation.
        let src = "x: str = \"'''\"\ny: int = 1  #pack\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("# pack"),
            "subsequent comment must be normalised since the previous \
             line did not actually open a triple-quoted block; got: {:?}",
            result.output
        );
    }

    #[test]
    fn detect_triple_quote_open_ignores_triples_inside_regular_strings() {
        assert_eq!(detect_triple_quote_open("x = \"'''\""), None);
        assert_eq!(detect_triple_quote_open("x = '\"\"\"'"), None);
        // But a real triple-quote opener still produces Some.
        assert_eq!(detect_triple_quote_open("x = \"\"\"hi"), Some('"'));
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
