//! Pre-processing pass: strip Typhon-specific syntax so the underlying
//! Python parser can handle the source.
//!
//! Transformations performed here:
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
//! 3. **`model X:` class declarations** — `model ClassName:` is rewritten to
//!    `class ClassName(BaseModel):`. The preprocessor records the line index
//!    so the formatter can restore the `model` keyword.
//!
//! 4. **`comptime val/var X: T = expr`** — the `comptime` prefix (and the
//!    immediately following `val`/`var`) is stripped, leaving `X: T = expr`.
//!    The binding name and line index are recorded so the build command can
//!    evaluate the RHS at compile time and inline the result.
//!
//! A separate function [`expand_question_ops`] rewrites `expr?` (the error-
//! propagation operator) into the multi-statement guard form before the main
//! pass runs. It is called explicitly by pipeline commands that need valid
//! Python output (`tyc build`, `tyc check`) but *not* by `tyc fmt`, which
//! must preserve the Typhon source unchanged.
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

/// A `comptime` binding whose RHS must be evaluated at build time.
#[derive(Debug, Clone)]
pub struct ComptimeBinding {
    /// The name of the comptime variable (e.g. `PORT` from `comptime val PORT: int = …`).
    pub name: String,
    /// 0-based line index of the binding in the **preprocessed** source (after
    /// keyword stripping). This is identical to the original-source line index
    /// because keyword stripping never changes the line count.
    pub line_index: usize,
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
    /// Bindings that were declared `comptime` and whose RHS the build command
    /// must evaluate and inline.
    pub comptime_bindings: Vec<ComptimeBinding>,
}

/// Strip Typhon-specific syntax from `source` and return the Python-
/// compatible string together with restoration metadata.
pub fn preprocess(source: &str) -> PreprocessResult {
    let mut python_source = String::with_capacity(source.len());
    let mut stripped = Vec::new();
    let mut optionals = Vec::new();
    let mut comptime_bindings = Vec::new();
    // String state carried across lines (triple-quoted strings may span them).
    let mut in_string: Option<StringMode> = None;

    for (line_index, line) in source.split_inclusive('\n').enumerate() {
        // Keyword stripping only applies outside of string content.
        let line_after_keyword = if in_string.is_none() {
            let indent_len = line
                .find(|c: char| !c.is_whitespace())
                .unwrap_or(line.len());
            let indent = &line[..indent_len];
            let rest = &line[indent_len..];

            // ── `model ClassName:` → `class ClassName(BaseModel):` ──────────
            if rest.starts_with("model ")
                && rest.len() > "model ".len()
                && (rest.as_bytes()["model ".len()].is_ascii_alphanumeric()
                    || rest.as_bytes()["model ".len()] == b'_')
            {
                let after_model = &rest["model ".len()..]; // e.g. "User:\n"
                // Find the `:` that opens the class body (last `:` not inside
                // parentheses/brackets on this line).
                if let Some(class_body) = make_model_class_line(after_model) {
                    stripped.push(StrippedKeyword {
                        line_index,
                        keyword: TyphonKeyword::Model,
                    });
                    let new_line = format!("{}class {}", indent, class_body);
                    // Fall through to optional rewriting with the new line.
                    let (rewritten, marks) = rewrite_optionals(&new_line, &mut in_string);
                    for col in marks {
                        optionals.push(StrippedOptional {
                            line_index,
                            python_col: col,
                        });
                    }
                    python_source.push_str(&rewritten);
                    continue;
                }
            }

            // ── `comptime val/var name…` → strip both prefixes ──────────────
            // `comptime` is a module-level concept; bindings inside functions
            // or classes cannot be evaluated at build time. Only record
            // top-level (indent_len == 0) comptime declarations.
            let mut stripped_line: Option<String> = None;
            if indent_len == 0 && rest.starts_with("comptime ") {
                let after_comptime = &rest["comptime ".len()..];
                // Extract the inner keyword (val/var) if present.
                let (inner_kw, payload) =
                    if after_comptime.starts_with("val ")
                        && after_comptime.len() > 4
                    {
                        (Some(TyphonKeyword::Val), &after_comptime["val ".len()..])
                    } else if after_comptime.starts_with("var ")
                        && after_comptime.len() > 4
                    {
                        (Some(TyphonKeyword::Var), &after_comptime["var ".len()..])
                    } else {
                        (None, after_comptime)
                    };

                // Extract the binding name (first identifier before `:` or `=`).
                let binding_name = payload
                    .split(|c: char| c == ':' || c == '=')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_owned();

                if !binding_name.is_empty() {
                    comptime_bindings.push(ComptimeBinding {
                        name: binding_name,
                        line_index,
                    });
                    // Record inner val/var first so postprocess restores it
                    // before prepending `comptime`.
                    if let Some(kw) = inner_kw {
                        stripped.push(StrippedKeyword {
                            line_index,
                            keyword: kw,
                        });
                    }
                    stripped.push(StrippedKeyword {
                        line_index,
                        keyword: TyphonKeyword::Comptime,
                    });
                    stripped_line = Some(format!("{}{}", indent, payload));
                }
            }

            // ── `val name…` / `var name…` → strip keyword ──────────────────
            if stripped_line.is_none() {
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
                        let after_kw = &rest[prefix.len() + 1..];
                        stripped_line = Some(format!("{}{}", indent, after_kw));
                        break;
                    }
                }
            }

            stripped_line.unwrap_or_else(|| line.to_owned())
        } else {
            line.to_owned()
        };

        let (rewritten, marks) = rewrite_optionals(&line_after_keyword, &mut in_string);
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
        comptime_bindings,
    }
}

/// Build the class-header portion of a `model` line, converting
/// `ClassName:\n` (or `ClassName :\n`) into `ClassName(BaseModel):\n`.
///
/// Returns `None` when the line doesn't look like a class header (no `:`).
fn make_model_class_line(after_model: &str) -> Option<String> {
    // Find the `:` that terminates the class header at depth 0.
    let mut depth = 0i32;
    let mut colon_pos = None;
    for (i, c) in after_model.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ':' if depth == 0 => {
                colon_pos = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon_pos = colon_pos?;
    let name = after_model[..colon_pos].trim_end();
    let tail = &after_model[colon_pos..]; // ":\n" or ":"
    // If `name` already ends with `)` it has existing bases — merge BaseModel
    // into the list.  Otherwise wrap with `(BaseModel)`.
    let new_name = if name.ends_with(')') {
        format!("{}, BaseModel)", &name[..name.len() - 1])
    } else {
        format!("{}(BaseModel)", name)
    };
    Some(format!("{}{}", new_name, tail))
}

/// Active string-literal mode while scanning. `Single` and `Double` cannot
/// span a newline (per Python's grammar) and are reset at end-of-line;
/// triple-quoted forms persist across lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StringMode {
    Single,
    Double,
    TripleSingle,
    TripleDouble,
}

/// Replace every `?` outside string literals and comments with ` | None`.
///
/// Returns the rewritten line and a list of 0-based byte columns in the
/// rewritten string where each ` | None` insertion *starts* (i.e. the
/// character right after the type token). The `in_string` argument tracks
/// triple-quoted string state across lines.
///
/// Known limitation: `?` inside an f-string expression (e.g. `f"{x?}"`) is
/// treated as string content and not rewritten — supporting it requires a
/// nested expression scanner, which is deferred.
fn rewrite_optionals(
    line: &str,
    in_string: &mut Option<StringMode>,
) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(line.len());
    let mut marks = Vec::new();
    let mut last_char: Option<char> = None;
    let mut chars = line.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        // Comment outside any string — copy the remainder verbatim.
        if in_string.is_none() && c == '#' {
            out.push(c);
            for (_, c2) in chars.by_ref() {
                out.push(c2);
            }
            break;
        }

        // Inside a string literal — copy chars, look for the close.
        if let Some(mode) = *in_string {
            out.push(c);
            // Backslash escape in single/double-quoted strings (but not raw —
            // we don't try to distinguish, since copying the next char
            // through is harmless either way).
            if matches!(mode, StringMode::Single | StringMode::Double) && c == '\\' {
                if let Some((_, esc)) = chars.next() {
                    out.push(esc);
                    last_char = Some(esc);
                    continue;
                }
            }
            match mode {
                StringMode::Single if c == '\'' => *in_string = None,
                StringMode::Double if c == '"' => *in_string = None,
                StringMode::TripleSingle if c == '\'' && line[i..].starts_with("'''") => {
                    out.push(chars.next().unwrap().1);
                    out.push(chars.next().unwrap().1);
                    *in_string = None;
                }
                StringMode::TripleDouble if c == '"' && line[i..].starts_with("\"\"\"") => {
                    out.push(chars.next().unwrap().1);
                    out.push(chars.next().unwrap().1);
                    *in_string = None;
                }
                _ => {}
            }
            last_char = Some(c);
            continue;
        }

        // Outside any string — detect a string opening.
        if c == '"' || c == '\'' {
            let triple = (c == '"' && line[i..].starts_with("\"\"\""))
                || (c == '\'' && line[i..].starts_with("'''"));
            out.push(c);
            if triple {
                out.push(chars.next().unwrap().1);
                out.push(chars.next().unwrap().1);
                *in_string = Some(if c == '"' {
                    StringMode::TripleDouble
                } else {
                    StringMode::TripleSingle
                });
            } else {
                *in_string = Some(if c == '"' {
                    StringMode::Double
                } else {
                    StringMode::Single
                });
            }
            last_char = Some(c);
            continue;
        }

        if c == '?' {
            let is_type_tail = matches!(
                last_char,
                Some(ch) if ch.is_alphanumeric() || ch == '_' || ch == ']'
            );
            if is_type_tail {
                marks.push(out.len());
                out.push_str(" | None");
                last_char = Some('e'); // last char of " | None"
                continue;
            }
            // Pass through unchanged; parser will surface a syntax error.
            out.push('?');
            last_char = Some('?');
            continue;
        }

        out.push(c);
        last_char = Some(c);
    }

    // Single/double-quoted strings cannot span a newline.
    if matches!(*in_string, Some(StringMode::Single) | Some(StringMode::Double)) {
        *in_string = None;
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

    // Restore Typhon keywords, processing from the last line to the first so
    // that line indices stay valid.
    let mut insertions: Vec<(usize, TyphonKeyword)> =
        stripped.iter().map(|sk| (sk.line_index, sk.keyword)).collect();
    // Sort descending by line_index; for identical line_index values preserve
    // original order (stable sort) so that `val` is restored before `comptime`
    // on the same line, allowing `comptime` to be prepended on top.
    insertions.sort_by(|a, b| b.0.cmp(&a.0));
    for (line_idx, kw) in insertions {
        if line_idx >= lines.len() {
            continue;
        }
        match kw {
            TyphonKeyword::Model => {
                // Restore `class X(BaseModel):` → `model X:`.
                let line = &lines[line_idx];
                let indent_len = line
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(line.len());
                let content = &line[indent_len..];
                let restored = if let Some(tail) = content.strip_prefix("class ") {
                    // Remove "(BaseModel)" inserted by preprocessing.
                    let tail = tail
                        .replacen("(BaseModel)", "", 1)
                        .replacen("(BaseModel, ", "(", 1);
                    format!("model {}", tail)
                } else {
                    format!("model {}", content)
                };
                lines[line_idx] = format!("{}{}", &line[..indent_len], restored);
            }
            TyphonKeyword::Val | TyphonKeyword::Var | TyphonKeyword::Comptime => {
                // Prepend the keyword (and a space) before the non-whitespace content.
                let line = &lines[line_idx];
                let indent_len = line
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(line.len());
                let new_line = format!(
                    "{}{} {}",
                    &line[..indent_len],
                    kw.as_str(),
                    &line[indent_len..]
                );
                lines[line_idx] = new_line;
            }
        }
    }

    let mut result = lines.join("\n");
    if has_trailing_newline {
        result.push('\n');
    }
    result
}

// ── `?` operator expansion ────────────────────────────────────────────────────

/// Expand the `?` error-propagation operator into equivalent Python guard code.
///
/// A line ending with `)?` in the code portion (before any comment) is treated
/// as the propagation operator. It is expanded in-place into the
/// three-statement guard form:
///
/// ```text
/// # Input (Typhon)
/// val x = f()?
///
/// # Output (valid Python, before further preprocessing)
/// __typhon_q_0__ = f()
/// if isinstance(__typhon_q_0__, Err):
///     return __typhon_q_0__
/// val x = __typhon_q_0__.value
/// ```
///
/// If there is no assignment target (bare `f()?`), the final assignment is
/// omitted and only the call + guard are emitted.
///
/// Lines inside triple-quoted strings and comment text are not expanded.
/// Trailing comments (`f()? # note`) are stripped before the check so the
/// comment does not interfere with operator detection.
///
/// This function is called **before** [`preprocess`] in the build and check
/// pipelines. It is deliberately *not* called by `tyc fmt` so that the
/// source formatter preserves the `?` syntax unchanged.
///
/// # Limitations
///
/// - Only handles `?` that directly follows `)`.  A `?` after `]` or an
///   identifier is treated as nullable-type sugar (`T?`) by the regular
///   preprocessor.
/// - Nested `?` operators on a single line are not supported.  Break them
///   across multiple `val` bindings instead.
pub fn expand_question_ops(source: &str) -> String {
    let mut result = String::with_capacity(source.len() + 64);
    let mut counter = 0usize;
    let mut in_string: Option<StringMode> = None;

    for line in source.split_inclusive('\n') {
        let raw = line.trim_end_matches(|c: char| c == '\n' || c == '\r');

        // Record string state at the start of this line.
        let pre_string = in_string;

        // Scan the line to update string state and find the code end position
        // (where a comment begins, or line.len() if no comment).
        let code_end = scan_line_code_end(raw, &mut in_string);

        // Lines that start inside a triple-quoted string are pure string
        // content — emit verbatim.  (Single-line strings can't span lines.)
        if pre_string.is_some() {
            result.push_str(line);
            continue;
        }

        // The effective code (before any comment), trimmed of trailing whitespace.
        let content = raw[..code_end].trim_end();

        // A `?` operator must be the very last non-whitespace character in the
        // code portion.
        if !content.ends_with('?') || content.is_empty() {
            result.push_str(line);
            continue;
        }

        // The character before the `?` must be `)` for it to be the propagation
        // operator.  `T?` (nullable sugar) follows an alphanumeric/`_`/`]` char
        // and is left alone.
        let before_q = &content[..content.len() - 1];
        if !matches!(before_q.chars().last(), Some(')')) {
            result.push_str(line);
            continue;
        }

        // Compute indentation to reproduce the correct nesting.
        let indent_len = content.find(|c: char| !c.is_whitespace()).unwrap_or(0);
        let indent = &content[..indent_len];

        // `expr_part` is the code content without the trailing `?`.
        let expr_part = &content[indent_len..content.len() - 1]; // e.g. "val x = f()"

        // Split into optional assignment LHS and the RHS expression.
        let (lhs, rhs) = match find_assignment_eq(expr_part) {
            Some(eq_pos) => {
                let l = expr_part[..eq_pos].trim();
                let r = expr_part[eq_pos + 1..].trim();
                (Some(l.to_owned()), r.to_owned())
            }
            None => (None, expr_part.to_owned()),
        };

        // Generate a unique temporary variable name.
        let tmp = format!("__typhon_q_{counter}__");
        counter += 1;

        let nl = if line.ends_with('\n') { "\n" } else { "" };

        // 1. Evaluate the expression into the temporary.
        result.push_str(indent);
        result.push_str(&tmp);
        result.push_str(" = ");
        result.push_str(&rhs);
        result.push('\n');

        // 2. Short-circuit on Err.
        result.push_str(indent);
        result.push_str("if isinstance(");
        result.push_str(&tmp);
        result.push_str(", Err):\n");
        result.push_str(indent);
        result.push_str("    return ");
        result.push_str(&tmp);
        result.push('\n');

        // 3. Bind the Ok value if there is an assignment target.
        if let Some(l) = lhs {
            result.push_str(indent);
            result.push_str(&l);
            result.push_str(" = ");
            result.push_str(&tmp);
            result.push_str(".value");
            result.push_str(nl);
        }
    }

    result
}

/// Scan `line` while updating `in_string` for string-literal state.
/// Returns the byte index where the "code" portion ends — the start of a
/// line comment (`#`) or `line.len()` if there is no comment.
///
/// Single- and double-quoted strings that do not close by the end of the
/// physical line are reset to `None` (Python syntax error territory anyway).
fn scan_line_code_end(line: &str, in_string: &mut Option<StringMode>) -> usize {
    let mut chars = line.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        if let Some(mode) = *in_string {
            match mode {
                // Single-line strings: handle backslash escapes and closing quote.
                StringMode::Single | StringMode::Double => {
                    if c == '\\' {
                        chars.next(); // skip escaped character
                        continue;
                    }
                    match mode {
                        StringMode::Single if c == '\'' => *in_string = None,
                        StringMode::Double if c == '"' => *in_string = None,
                        _ => {}
                    }
                }
                // Triple-quoted strings: look for the three-char closing sequence.
                StringMode::TripleSingle | StringMode::TripleDouble => {
                    let (q, triple) = if matches!(mode, StringMode::TripleSingle) {
                        ('\'', "'''")
                    } else {
                        ('"', "\"\"\"")
                    };
                    if c == q && line[i..].starts_with(triple) {
                        chars.next();
                        chars.next();
                        *in_string = None;
                    }
                }
            }
            continue;
        }

        // Outside any string.
        if c == '#' {
            return i; // rest of line is a comment
        }

        if c == '"' || c == '\'' {
            let triple = (c == '"' && line[i..].starts_with("\"\"\""))
                || (c == '\'' && line[i..].starts_with("'''"));
            if triple {
                chars.next();
                chars.next();
                *in_string = Some(if c == '"' {
                    StringMode::TripleDouble
                } else {
                    StringMode::TripleSingle
                });
            } else {
                *in_string = Some(if c == '"' {
                    StringMode::Double
                } else {
                    StringMode::Single
                });
            }
        }
    }

    // A single/double-quoted string that didn't close before end-of-line is
    // a Python syntax error — reset so subsequent lines parse correctly.
    if matches!(*in_string, Some(StringMode::Single | StringMode::Double)) {
        *in_string = None;
    }

    line.len()
}

/// Find the position of the `=` assignment operator in `s`, ignoring `=`
/// characters inside parentheses/brackets/strings and `==` comparisons.
fn find_assignment_eq(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_str: Option<char> = None;
    let mut chars = s.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        if let Some(q) = in_str {
            if c == q {
                in_str = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => in_str = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '=' if depth == 0 => {
                // Reject `==`, `!=`, `<=`, `>=`.
                let prev = if i > 0 { s[..i].chars().last() } else { None };
                let next = s[i + 1..].chars().next();
                if !matches!(prev, Some('!' | '<' | '>' | '='))
                    && !matches!(next, Some('='))
                {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
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
    fn does_not_rewrite_question_mark_in_triple_quoted_string() {
        let src = "val s: str = \"\"\"line one\nline two with ?\nline three\"\"\"\n";
        let result = preprocess(src);
        // The ? inside the triple-quoted block must be preserved as-is.
        assert!(result.python_source.contains("line two with ?"));
        assert!(result.optionals.is_empty());
    }

    #[test]
    fn handles_non_ascii_source_without_corruption() {
        // Em-dash and accented characters must survive char-aware iteration.
        let src = "# café — entry point\nval name: str? = None\n";
        let result = preprocess(src);
        assert!(result.python_source.contains("café — entry point"));
        assert!(result.python_source.contains("name: str | None"));
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

    // ── model keyword ────────────────────────────────────────────────────────

    #[test]
    fn model_keyword_becomes_basemodel_class() {
        let result = preprocess("model User:\n    id: int\n");
        assert!(
            result.python_source.contains("class User(BaseModel):"),
            "output: {}",
            result.python_source
        );
        assert!(result.stripped.iter().any(|k| matches!(k.keyword, TyphonKeyword::Model)));
    }

    #[test]
    fn model_keyword_round_trips_via_postprocess() {
        let src = "model User:\n    id: int\n";
        let prep = preprocess(src);
        let out = postprocess(&prep.python_source, &prep.stripped, &prep.optionals);
        assert_eq!(out, src);
    }

    #[test]
    fn indented_model_class_preserved() {
        let src = "    model Inner:\n        x: int\n";
        let result = preprocess(src);
        assert!(result.python_source.contains("class Inner(BaseModel):"), "output: {}", result.python_source);
    }

    // ── comptime keyword ────────────────────────────────────────────────────

    #[test]
    fn comptime_val_stripped_to_plain_assignment() {
        let result = preprocess("comptime val PORT: int = 8080\n");
        assert_eq!(result.python_source, "PORT: int = 8080\n");
        assert_eq!(result.comptime_bindings.len(), 1);
        assert_eq!(result.comptime_bindings[0].name, "PORT");
    }

    #[test]
    fn comptime_var_stripped_correctly() {
        let result = preprocess("comptime var DB_URL: str = \"postgres://localhost\"\n");
        assert_eq!(result.python_source, "DB_URL: str = \"postgres://localhost\"\n");
        assert_eq!(result.comptime_bindings[0].name, "DB_URL");
    }

    #[test]
    fn comptime_val_round_trips_via_postprocess() {
        let src = "comptime val PORT: int = 8080\n";
        let prep = preprocess(src);
        let out = postprocess(&prep.python_source, &prep.stripped, &prep.optionals);
        assert_eq!(out, src);
    }

    // ── ? operator expansion ────────────────────────────────────────────────

    #[test]
    fn question_op_expands_assignment() {
        let src = "val x = f()?\n";
        let out = expand_question_ops(src);
        assert!(out.contains("__typhon_q_0__ = f()"), "out: {out}");
        assert!(out.contains("if isinstance(__typhon_q_0__, Err):"), "out: {out}");
        assert!(out.contains("return __typhon_q_0__"), "out: {out}");
        assert!(out.contains("val x = __typhon_q_0__.value"), "out: {out}");
    }

    #[test]
    fn question_op_expands_bare_call() {
        let src = "save(record)?\n";
        let out = expand_question_ops(src);
        assert!(out.contains("__typhon_q_0__ = save(record)"), "out: {out}");
        assert!(out.contains("if isinstance(__typhon_q_0__, Err):"), "out: {out}");
        // No binding assignment for a bare expression.
        assert!(!out.contains(".value"), "out: {out}");
    }

    #[test]
    fn type_annotation_nullable_not_treated_as_op() {
        // `str?` ends with `?` but last char before `?` is `r` (alphanumeric),
        // so it should NOT be treated as the propagation operator.
        let src = "val x: str? = None\n";
        let out = expand_question_ops(src);
        assert_eq!(out, src, "type sugar must be preserved unchanged");
    }

    #[test]
    fn question_op_preserves_indent() {
        let src = "    val y = compute()?\n";
        let out = expand_question_ops(src);
        assert!(out.contains("    __typhon_q_0__ = compute()"), "out: {out}");
        assert!(out.contains("    if isinstance"), "out: {out}");
        assert!(out.contains("    val y = __typhon_q_0__.value"), "out: {out}");
    }

    #[test]
    fn question_op_preserves_lhs_with_type_annotation() {
        let src = "val result: int = compute()?\n";
        let out = expand_question_ops(src);
        assert!(out.contains("val result: int = __typhon_q_0__.value"), "out: {out}");
    }

    #[test]
    fn question_op_ignores_comment_line() {
        // A comment that mentions `)?` must not trigger expansion.
        let src = "# should we call f()?\nx: int = 1\n";
        let out = expand_question_ops(src);
        assert_eq!(out, src, "comment line must be copied verbatim");
    }

    #[test]
    fn question_op_ignores_trailing_comment() {
        // Trailing comment after `)` must not be seen as `)?`.
        let src = "x = f() # returns f()?\n";
        let out = expand_question_ops(src);
        assert_eq!(out, src, "trailing comment with ')?' must not trigger expansion");
    }

    #[test]
    fn question_op_ignores_triple_string_content() {
        // `)?` inside a triple-quoted string must not be expanded.
        let src = "msg = \"\"\"\ncall f()?\n\"\"\"\n";
        let out = expand_question_ops(src);
        assert_eq!(out, src, "triple-string content must not be expanded");
    }

    #[test]
    fn model_keyword_with_existing_base_merges_basemodel() {
        // `model User(Timestamped):` → `class User(Timestamped, BaseModel):`
        let result = preprocess("model User(Timestamped):\n    id: int\n");
        assert!(
            result.python_source.contains("class User(Timestamped, BaseModel):"),
            "got: {}",
            result.python_source
        );
    }

    #[test]
    fn model_keyword_underscore_name() {
        // `model _Internal:` should be rewritten (underscore-starting names are valid).
        let result = preprocess("model _Internal:\n    x: int\n");
        assert!(
            result.python_source.contains("class _Internal(BaseModel):"),
            "got: {}",
            result.python_source
        );
    }

    #[test]
    fn comptime_inside_function_not_recorded() {
        // `comptime val` inside a function should NOT be recorded as a comptime
        // binding (it would fail evaluation anyway since it's not top-level).
        let src = "def f():\n    comptime val X: int = 1\n";
        let result = preprocess(src);
        assert!(
            result.comptime_bindings.is_empty(),
            "indented comptime should not be recorded: {:?}",
            result.comptime_bindings
        );
    }
}
