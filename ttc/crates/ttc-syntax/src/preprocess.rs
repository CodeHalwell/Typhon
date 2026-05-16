//! Pre-processing pass: strip Typhon-specific syntax so the underlying
//! Python parser can handle the source.
//!
//! Two transformations are performed here:
//!
//! 1. **`val` / `var` line prefixes** — `val x: T = expr` and
//!    `var x: T = expr` are reduced to `x: T = expr`. The stripped keyword
//!    is recorded so the formatter can restore it.
//!
//! 2. **`T?` nullability sugar** — every `?` outside a string or comment is
//!    replaced with ` | None`, so an annotation like `email: str?` becomes
//!    `email: str | None`. Positions are recorded so the formatter can
//!    restore the `?` sugar exactly.
//!
//! Anything else is left untouched.

use crate::lexer::TyphonKeyword;

/// One stripped keyword and the 0-based line index in the source where it
/// appeared.
#[derive(Debug, Clone)]
pub struct StrippedKeyword {
    /// 0-based line index in both the original source and the preprocessed
    /// source (stripping a keyword never changes the line count).
    pub line_index: usize,
    pub keyword: TyphonKeyword,
}

/// A `?` that was rewritten to ` | None` during preprocessing, with the
/// information needed to put it back.
#[derive(Debug, Clone)]
pub struct StrippedOptional {
    /// 0-based line index of the original `?`.
    pub line_index: usize,
    /// 0-based column where the `T | None` substitution begins on that line
    /// in the *pre-processed* source (i.e. immediately after the type name).
    pub python_col: usize,
}

/// Result of pre-processing a Typhon source file.
#[derive(Debug)]
pub struct PreprocessResult {
    /// The Python-compatible source (val/var stripped, `?` rewritten).
    pub python_source: String,
    /// Metadata about every stripped keyword, in source order.
    pub stripped: Vec<StrippedKeyword>,
    /// Metadata about every `?` that was rewritten, in source order.
    pub optionals: Vec<StrippedOptional>,
}

/// Strip Typhon-specific syntax from `source` and return the Python-
/// compatible string together with restoration metadata.
pub fn preprocess(source: &str) -> PreprocessResult {
    let mut python_source = String::with_capacity(source.len());
    let mut stripped = Vec::new();
    let mut optionals = Vec::new();

    for (line_index, line) in source.split_inclusive('\n').enumerate() {
        // --- val / var stripping at start of line ---
        let indent_len = line
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(line.len());
        let rest = &line[indent_len..];

        let (line_after_keyword, indent_len_after_keyword) = {
            let mut consumed_keyword = false;
            let mut out_indent = indent_len;
            let mut out_line = line.to_owned();
            for kw in &[TyphonKeyword::Val, TyphonKeyword::Var] {
                let prefix = kw.as_str();
                if rest.starts_with(prefix)
                    && rest.len() > prefix.len()
                    && rest.as_bytes()[prefix.len()] == b' '
                {
                    stripped.push(StrippedKeyword {
                        line_index,
                        keyword: *kw,
                    });
                    let indent = &line[..indent_len];
                    let after_kw = &rest[prefix.len() + 1..];
                    out_line = format!("{}{}", indent, after_kw);
                    out_indent = indent_len;
                    consumed_keyword = true;
                    break;
                }
            }
            let _ = consumed_keyword;
            (out_line, out_indent)
        };

        let _ = indent_len_after_keyword;

        // --- `?` → ` | None` substitution (string- and comment-aware) ---
        let (rewritten, marks) = rewrite_optionals(&line_after_keyword);
        for col in marks {
            optionals.push(StrippedOptional {
                line_index,
                python_col: col,
            });
        }
        python_source.push_str(&rewritten);
    }

    PreprocessResult {
        python_source,
        stripped,
        optionals,
    }
}

/// Replace every `?` outside string literals and comments with ` | None`.
///
/// Returns the rewritten line and a list of 0-based byte columns in the
/// rewritten string where each `| None` insertion *starts* (i.e. the
/// character right after the type token).
fn rewrite_optionals(line: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(line.len());
    let mut marks = Vec::new();

    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_string: Option<u8> = None; // active quote character

    while i < bytes.len() {
        let b = bytes[i];

        // Comment — copy rest of line as-is.
        if in_string.is_none() && b == b'#' {
            out.push_str(&line[i..]);
            break;
        }

        // Track string state.
        if let Some(quote) = in_string {
            out.push(b as char);
            if b == b'\\' && i + 1 < bytes.len() {
                // Copy the escaped byte literally.
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if b == quote {
                in_string = None;
            }
            i += 1;
            continue;
        }

        if b == b'"' || b == b'\'' {
            in_string = Some(b);
            out.push(b as char);
            i += 1;
            continue;
        }

        if b == b'?' {
            // Skip a stray `?` that is not preceded by something that could
            // be the tail of a type expression. In Phase 1 we accept `?`
            // after an identifier character or `]` only.
            let prev_char = out.chars().last();
            let is_type_tail = matches!(prev_char, Some(c) if c.is_alphanumeric() || c == '_' || c == ']');
            if is_type_tail {
                let mark = out.len();
                out.push_str(" | None");
                marks.push(mark);
                i += 1;
                continue;
            } else {
                // Pass it through unchanged; the parser will report a syntax
                // error so the user sees a clear diagnostic.
                out.push('?');
                i += 1;
                continue;
            }
        }

        out.push(b as char);
        i += 1;
    }

    (out, marks)
}

/// Restore stripped keywords and `?` sugar into a normalised Python source
/// string.
///
/// `normalised` is the Python source after whitespace normalisation.
pub fn postprocess(
    normalised: &str,
    stripped: &[StrippedKeyword],
    optionals: &[StrippedOptional],
) -> String {
    if stripped.is_empty() && optionals.is_empty() {
        return normalised.to_owned();
    }

    let mut lines: Vec<String> = normalised.lines().map(|l| l.to_owned()).collect();
    let has_trailing_newline = normalised.ends_with('\n');

    // Restore `?` first (right-to-left within each line), then val/var, so
    // column offsets stay valid.
    let mut per_line: std::collections::HashMap<usize, Vec<usize>> =
        std::collections::HashMap::new();
    for opt in optionals {
        per_line.entry(opt.line_index).or_default().push(opt.python_col);
    }
    for (line_idx, mut cols) in per_line {
        if line_idx >= lines.len() {
            continue;
        }
        cols.sort_unstable_by(|a, b| b.cmp(a));
        let mut line = std::mem::take(&mut lines[line_idx]);
        for col in cols {
            // Defensive bounds checks — normalisation can move columns
            // around (it doesn't in Phase 0, but stay safe).
            const REWRITE: &str = " | None";
            if col <= line.len() && line[col..].starts_with(REWRITE) {
                line.replace_range(col..col + REWRITE.len(), "?");
            }
        }
        lines[line_idx] = line;
    }

    // Now restore val/var keywords.
    let mut insertions: Vec<(usize, TyphonKeyword)> =
        stripped.iter().map(|sk| (sk.line_index, sk.keyword)).collect();
    insertions.sort_by(|a, b| b.0.cmp(&a.0));
    for (line_idx, kw) in insertions {
        if line_idx < lines.len() {
            let line = &lines[line_idx];
            let indent_len = line
                .find(|c: char| !c.is_whitespace())
                .unwrap_or(line.len());
            let new_line =
                format!("{}{} {}", &line[..indent_len], kw.as_str(), &line[indent_len..]);
            lines[line_idx] = new_line;
        }
    }

    let mut result = lines.join("\n");
    if has_trailing_newline {
        result.push('\n');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_val() {
        let result = preprocess("val x: int = 1\n");
        assert_eq!(result.python_source, "x: int = 1\n");
        assert_eq!(result.stripped.len(), 1);
        assert!(matches!(result.stripped[0].keyword, TyphonKeyword::Val));
        assert_eq!(result.stripped[0].line_index, 0);
    }

    #[test]
    fn strips_var() {
        let result = preprocess("var count: int = 0\n");
        assert_eq!(result.python_source, "count: int = 0\n");
        assert_eq!(result.stripped.len(), 1);
        assert!(matches!(result.stripped[0].keyword, TyphonKeyword::Var));
    }

    #[test]
    fn strips_indented_val() {
        let result = preprocess("    val x: int = 1\n");
        assert_eq!(result.python_source, "    x: int = 1\n");
    }

    #[test]
    fn rewrites_optional_in_annotation() {
        let result = preprocess("val email: str? = None\n");
        assert_eq!(result.python_source, "email: str | None = None\n");
        assert_eq!(result.optionals.len(), 1);
        // The optional starts at column 11 ("email: str|").
        assert_eq!(result.optionals[0].python_col, 10);
    }

    #[test]
    fn rewrites_optional_after_subscript() {
        let result = preprocess("x: list[int]? = None\n");
        assert_eq!(result.python_source, "x: list[int] | None = None\n");
        assert_eq!(result.optionals.len(), 1);
    }

    #[test]
    fn does_not_rewrite_question_mark_inside_string() {
        let result = preprocess("val s: str = \"is this ok?\"\n");
        assert_eq!(result.python_source, "s: str = \"is this ok?\"\n");
        assert!(result.optionals.is_empty());
    }

    #[test]
    fn does_not_rewrite_question_mark_inside_comment() {
        let result = preprocess("x: int = 1  # really?\n");
        assert_eq!(result.python_source, "x: int = 1  # really?\n");
        assert!(result.optionals.is_empty());
    }

    #[test]
    fn round_trips_optional() {
        let src = "val email: str? = None\n";
        let prep = preprocess(src);
        let out = postprocess(&prep.python_source, &prep.stripped, &prep.optionals);
        assert_eq!(out, src);
    }

    #[test]
    fn plain_python_unchanged() {
        let src = "x: int = 1\n";
        let result = preprocess(src);
        assert_eq!(result.python_source, src);
        assert!(result.stripped.is_empty());
        assert!(result.optionals.is_empty());
    }
}
