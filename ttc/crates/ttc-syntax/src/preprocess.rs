//! Pre-processing pass: strip Typhon-specific keywords so that the underlying
//! Python parser can handle the source.
//!
//! `val x: T = expr` becomes `x: T = expr`
//! `var x: T = expr` becomes `x: T = expr`
//!
//! The returned [`PreprocessResult`] records the 0-based line index of every
//! removed keyword so that the post-processor can restore them exactly.

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

/// Result of pre-processing a Typhon source file.
#[derive(Debug)]
pub struct PreprocessResult {
    /// The Python-compatible source (val/var stripped).
    pub python_source: String,
    /// Metadata about every stripped keyword, in source order.
    pub stripped: Vec<StrippedKeyword>,
}

/// Strip Typhon-specific keywords from `source` and return the Python-
/// compatible string together with restoration metadata.
///
/// Strategy: process the source line by line.  For each line, if the
/// non-whitespace prefix is `val ` or `var `, strip the keyword and the
/// single space that follows it, keeping any leading indentation intact.
pub fn preprocess(source: &str) -> PreprocessResult {
    let mut python_source = String::with_capacity(source.len());
    let mut stripped = Vec::new();
    let mut line_index: usize = 0;

    for line in source.split_inclusive('\n') {
        // Find leading whitespace.
        let indent_len = line
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(line.len());
        let rest = &line[indent_len..];

        let mut found = false;
        for kw in &[TyphonKeyword::Val, TyphonKeyword::Var] {
            let prefix = kw.as_str(); // "val" or "var"
            // Check that the line starts with the keyword followed by a space.
            if rest.starts_with(prefix)
                && rest.len() > prefix.len()
                && rest.as_bytes()[prefix.len()] == b' '
            {
                // Emit the leading indentation unchanged.
                let indent = &line[..indent_len];
                python_source.push_str(indent);

                // Record the keyword at its 0-based line index.
                stripped.push(StrippedKeyword {
                    line_index,
                    keyword: *kw,
                });

                // Skip keyword + one space; emit the rest of the line.
                let after_kw = &rest[prefix.len() + 1..];
                python_source.push_str(after_kw);

                found = true;
                break;
            }
        }

        if !found {
            python_source.push_str(line);
        }

        line_index += 1;
    }

    PreprocessResult {
        python_source,
        stripped,
    }
}

/// Restore stripped keywords into a normalised Python source string.
///
/// `normalised` is the Python source after whitespace normalisation.
/// `stripped` is the metadata returned by [`preprocess`].
///
/// Because each [`StrippedKeyword`] stores the 0-based line index from the
/// original source, and stripping keywords never alters the line count, the
/// same index addresses the correct line in `normalised`.
pub fn postprocess(normalised: &str, stripped: &[StrippedKeyword]) -> String {
    if stripped.is_empty() {
        return normalised.to_owned();
    }

    // Build insertion list, sorted in reverse order so that later lines are
    // handled first (avoids index shifting problems if we ever join + split).
    let mut insertions: Vec<(usize, TyphonKeyword)> =
        stripped.iter().map(|sk| (sk.line_index, sk.keyword)).collect();
    insertions.sort_by(|a, b| b.0.cmp(&a.0));

    // Work with owned Strings to avoid lifetime issues.
    let mut lines: Vec<String> = normalised.lines().map(|l| l.to_owned()).collect();
    let has_trailing_newline = normalised.ends_with('\n');

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
        assert_eq!(result.stripped[0].line_index, 0);
    }

    #[test]
    fn strips_indented_val() {
        let result = preprocess("    val x: int = 1\n");
        assert_eq!(result.python_source, "    x: int = 1\n");
        assert_eq!(result.stripped.len(), 1);
        assert_eq!(result.stripped[0].line_index, 0);
    }

    #[test]
    fn strips_multiple_keywords_correct_line_indices() {
        let src = "val a: int = 1\nx: int = 2\nvar b: int = 3\n";
        let result = preprocess(src);
        assert_eq!(result.python_source, "a: int = 1\nx: int = 2\nb: int = 3\n");
        assert_eq!(result.stripped.len(), 2);
        assert_eq!(result.stripped[0].line_index, 0); // "val a" is on line 0
        assert_eq!(result.stripped[1].line_index, 2); // "var b" is on line 2
    }

    #[test]
    fn plain_python_unchanged() {
        let src = "x: int = 1\n";
        let result = preprocess(src);
        assert_eq!(result.python_source, src);
        assert!(result.stripped.is_empty());
    }
}
