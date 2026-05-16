//! Pre-processing pass: strip Typhon-specific syntax so the underlying
//! Python parser can handle the source.
//!
//! Transformations:
//!
//! 1. **`val` / `var` line prefixes** — `val x: T = expr` and
//!    `var x: T = expr` are reduced to `x: T = expr`. The stripped keyword
//!    is recorded so the formatter can restore it.
//!
//! 2. **`T?` nullability sugar** — every `?` outside a string or comment that
//!    follows an alphanumeric character, `_`, or `]` is replaced with ` | None`,
//!    so an annotation like `email: str?` becomes `email: str | None`. Positions
//!    are recorded so the formatter can restore the `?` sugar exactly.
//!
//! 3. **`model` keyword** — `model ClassName:` (Pydantic class syntax) is
//!    transformed to `class ClassName(__TyphonModel__):`. The desugar pass
//!    detects `__TyphonModel__` base classes and emits `class X(BaseModel):`.
//!
//! 4. **`comptime` keyword** — `comptime val X: T = env("NAME", "default")`
//!    is evaluated at preprocessing time. The `env()` call reads environment
//!    variables; `int(env(...))` and `str(env(...))` wrappers are supported.
//!    Missing required env vars are collected in `comptime_errors`.
//!
//! 5. **`?` try-operator** — `call()?` (where `?` follows `)`) is converted
//!    to `call().__typhon_try__()`. The desugar pass expands these into the
//!    `if isinstance(r, Err): return r` early-return pattern.

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
    /// The Python-compatible source (val/var stripped, `?` rewritten, etc.).
    pub python_source: String,
    /// Metadata about every stripped keyword, in source order.
    pub stripped: Vec<StrippedKeyword>,
    /// Metadata about every `?` that was rewritten to `| None`, in source order.
    pub optionals: Vec<StrippedOptional>,
    /// Errors from `comptime` evaluation, e.g. a required `env()` var that
    /// is not set. Each entry is a human-readable message. The build command
    /// should fail when this list is non-empty.
    pub comptime_errors: Vec<String>,
}

/// Strip Typhon-specific syntax from `source` and return the Python-
/// compatible string together with restoration metadata.
pub fn preprocess(source: &str) -> PreprocessResult {
    let mut python_source = String::with_capacity(source.len());
    let mut stripped = Vec::new();
    let mut optionals = Vec::new();
    let mut comptime_errors = Vec::new();
    // String state carried across lines (triple-quoted strings may span them).
    let mut in_string: Option<StringMode> = None;

    for (line_index, line) in source.split_inclusive('\n').enumerate() {
        // All Typhon-specific line transformations apply outside of string
        // content only.
        let line_after_keyword = if in_string.is_none() {
            // ── 1. model keyword ─────────────────────────────────────────────
            if let Some(model_line) = transform_model_line(line) {
                model_line
            }
            // ── 2. comptime keyword ──────────────────────────────────────────
            else if has_comptime_prefix(line) {
                match transform_comptime_line(line) {
                    Ok(evaluated) => evaluated,
                    Err(e) => {
                        comptime_errors.push(e);
                        // Produce a parseable fallback: strip `comptime val/var`
                        // and use `None` as the placeholder value, so the file
                        // still parses during the type-check phase. The build
                        // will abort on `comptime_errors` before emitting.
                        comptime_fallback_line(line)
                    }
                }
            }
            // ── 3. val / var stripping ────────────────────────────────────────
            else {
                let indent_len = line
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(line.len());
                let rest = &line[indent_len..];
                let mut stripped_line = None;
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
                        stripped_line = Some(format!("{}{}", indent, after_kw));
                        break;
                    }
                }
                stripped_line.unwrap_or_else(|| line.to_owned())
            }
        } else {
            line.to_owned()
        };

        // ── 4. `T?` → `T | None`  and  `call()?` → `call().__typhon_try__()` ──
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
        comptime_errors,
    }
}

// ── model keyword ─────────────────────────────────────────────────────────────

/// Transform `model ClassName:` to `class ClassName(__TyphonModel__):`.
///
/// Also handles `model ClassName(Base1, Base2):` →
/// `class ClassName(Base1, Base2, __TyphonModel__):`.
///
/// Returns `None` if the line is not a `model` statement.
fn transform_model_line(line: &str) -> Option<String> {
    let indent_len = line
        .find(|c: char| !c.is_whitespace())
        .unwrap_or(line.len());
    let rest = &line[indent_len..];

    // Must start with `model ` (keyword followed by a space).
    if !rest.starts_with("model ") {
        return None;
    }
    let after_model = &rest["model ".len()..];

    // Extract the class name (an identifier).
    let name_end = after_model
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(after_model.len());
    if name_end == 0 {
        return None;
    }
    let class_name = &after_model[..name_end];
    let after_name = after_model[name_end..].trim_start();

    let indent = &line[..indent_len];
    // Preserve any trailing content after the `:` (comments, newlines).
    let trail = trailing_content(after_name);

    let new_line = if let Some(rest_after_colon) = after_name.strip_prefix(':') {
        // Simple case: `model Name:` → `class Name(__TyphonModel__):`
        format!(
            "{}class {}(__TyphonModel__):{}",
            indent,
            class_name,
            rest_after_colon
        )
    } else if after_name.starts_with('(') {
        // With bases: `model Name(Base):` → `class Name(Base, __TyphonModel__):`
        // Find the matching `):`
        let inner = &after_name[1..]; // skip opening `(`
        if let Some(close) = find_closing_paren(inner) {
            let bases = &inner[..close];
            let rest_after_close = &inner[close + 1..]; // after `)`
            let rest_trimmed = rest_after_close.trim_start_matches(':');
            let separator = if bases.trim().is_empty() { "" } else { ", " };
            format!(
                "{}class {}({}{}__TyphonModel__):{}",
                indent,
                class_name,
                bases,
                separator,
                rest_trimmed
            )
        } else {
            return None; // malformed — let the parser report the error
        }
    } else {
        return None;
    };

    let _ = trail; // already embedded via rest_after_colon / rest_trimmed
    Some(new_line)
}

/// Return the content after the first `:` on the line (for trailing comments
/// and newlines), used to preserve them in transformed lines.
fn trailing_content(s: &str) -> &str {
    if let Some(pos) = s.find(':') {
        &s[pos + 1..]
    } else {
        ""
    }
}

/// Find the index of the `)` that closes the first `(` in `s` (where `s`
/// starts just *after* the opening `(`).
fn find_closing_paren(s: &str) -> Option<usize> {
    let mut depth = 1usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

// ── comptime keyword ──────────────────────────────────────────────────────────

/// Return `true` if the line (ignoring leading whitespace) starts with
/// `comptime ` followed by `val` or `var`.
fn has_comptime_prefix(line: &str) -> bool {
    let rest = line.trim_start();
    rest.starts_with("comptime val ") || rest.starts_with("comptime var ")
}

/// Transform `comptime val X: T = expr` by evaluating `expr` and returning
/// the rewritten line `val X: T = <literal>`.
///
/// Returns `Err(message)` when evaluation fails (e.g. missing required env
/// var); the caller records the message in `comptime_errors`.
fn transform_comptime_line(line: &str) -> Result<String, String> {
    let indent_len = line
        .find(|c: char| !c.is_whitespace())
        .unwrap_or(line.len());
    let rest = &line[indent_len..];
    let indent = &line[..indent_len];

    // Strip `comptime ` prefix, then strip `val ` / `var `.
    let after_comptime = rest
        .strip_prefix("comptime ")
        .ok_or_else(|| "internal: has_comptime_prefix mismatch".to_owned())?;

    // Remove the `val`/`var` keyword so the emitted line is plain Python.
    let after_kw = after_comptime
        .strip_prefix("val ")
        .or_else(|| after_comptime.strip_prefix("var "))
        .ok_or_else(|| format!("comptime: expected `val` or `var` after `comptime`"))?;

    // Find the `=` that separates the declaration from the expression.
    // We need to split at the first `=` that is not inside parentheses or
    // string literals. A simple scan is sufficient for the supported subset.
    let eq_pos = find_assign_eq(after_kw)
        .ok_or_else(|| format!("comptime: no `=` found in: {line}"))?;

    let decl = &after_kw[..eq_pos]; // e.g. `PORT: int `
    let expr = after_kw[eq_pos + 1..].trim(); // e.g. `int(env("PORT", "8080"))`

    // Evaluate the comptime expression.
    let literal = eval_comptime_expr(expr)?;

    // Preserve trailing newline/comment from the original line.
    let trail = if line.ends_with('\n') { "\n" } else { "" };

    Ok(format!(
        "{}{}= {}{}",
        indent,
        decl,
        literal,
        trail
    ))
}

/// Find the index of the `=` sign used as an assignment operator in `s`,
/// skipping over parenthesised sub-expressions and string literals.
fn find_assign_eq(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_str: Option<char> = None;
    let mut chars = s.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        if let Some(q) = in_str {
            if c == '\\' {
                chars.next(); // skip escaped char
            } else if c == q {
                in_str = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                in_str = Some(c);
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
            }
            '=' if depth == 0 => {
                // Skip `==` (comparison).
                if s.as_bytes().get(i + 1) != Some(&b'=') {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Evaluate a comptime expression to a Python literal string.
///
/// Supported forms:
/// - `env("NAME")` — required env var; error if missing
/// - `env("NAME", "DEFAULT")` — optional env var
/// - `int(env(...))` — evaluate env, then parse as integer
/// - `str(env(...))` — evaluate env, return as Python string literal
fn eval_comptime_expr(expr: &str) -> Result<String, String> {
    let expr = expr.trim();

    // `int(inner)`
    if let Some(inner) = strip_fn_call(expr, "int") {
        let s = eval_env_call(inner.trim())?;
        let n: i64 = s
            .trim()
            .parse()
            .map_err(|_| format!("comptime: cannot convert {s:?} to int"))?;
        return Ok(n.to_string());
    }

    // `str(inner)` — just evaluates env and returns a quoted string
    if let Some(inner) = strip_fn_call(expr, "str") {
        let s = eval_env_call(inner.trim())?;
        return Ok(python_string_literal(&s));
    }

    // Bare `env(...)` call
    let s = eval_env_call(expr)?;
    Ok(python_string_literal(&s))
}

/// If `s` is of the form `name(...)`, return the content between the outer
/// parentheses. Handles nested parens correctly.
fn strip_fn_call<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let prefix = format!("{}(", name);
    let rest = s.strip_prefix(prefix.as_str())?;
    if !s.ends_with(')') {
        return None;
    }
    // Find the matching close paren (the `s.ends_with(')')` we already
    // checked is the outermost close).
    Some(&rest[..rest.len() - 1])
}

/// Evaluate `env("NAME")` or `env("NAME", "DEFAULT")` using the process
/// environment at build time.
fn eval_env_call(s: &str) -> Result<String, String> {
    let s = s.trim();
    if !s.starts_with("env(") || !s.ends_with(')') {
        return Err(format!(
            "comptime: unsupported expression (only env() is supported): {s}"
        ));
    }
    let args_str = &s["env(".len()..s.len() - 1];
    let args = parse_py_string_args(args_str);

    match args.as_slice() {
        [name] => std::env::var(name)
            .map_err(|_| format!("comptime: required env var {name:?} is not set")),
        [name, default] => {
            Ok(std::env::var(name).unwrap_or_else(|_| default.clone()))
        }
        _ => Err(format!(
            "comptime: env() expects 1 or 2 string arguments, got: {s}"
        )),
    }
}

/// Parse a comma-separated list of Python string literals (single- or
/// double-quoted) and return the unquoted string values.
///
/// This covers the subset used in `env("NAME")` and `env("NAME", "default")`.
fn parse_py_string_args(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut chars = s.chars().peekable();

    loop {
        // Skip whitespace and commas between arguments.
        while let Some(&c) = chars.peek() {
            if c == ',' || c.is_whitespace() {
                chars.next();
            } else {
                break;
            }
        }

        // Read a string literal.
        let quote = match chars.peek() {
            Some(&'"') => '"',
            Some(&'\'') => '\'',
            _ => break,
        };
        chars.next(); // consume opening quote

        let mut value = String::new();
        loop {
            match chars.next() {
                None => break,
                Some('\\') => {
                    match chars.next() {
                        Some('n') => value.push('\n'),
                        Some('t') => value.push('\t'),
                        Some('\\') => value.push('\\'),
                        Some('"') => value.push('"'),
                        Some('\'') => value.push('\''),
                        Some(c) => {
                            value.push('\\');
                            value.push(c);
                        }
                        None => {}
                    }
                }
                Some(c) if c == quote => break,
                Some(c) => value.push(c),
            }
        }
        args.push(value);
    }

    args
}

/// Return a Python string literal for `s`, using double quotes and escaping
/// any double quotes and backslashes inside the value.
fn python_string_literal(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Produce a valid-Python fallback line for a `comptime val/var` binding
/// whose evaluation failed (e.g. missing required env var). Strips
/// `comptime val/var` and replaces the RHS with `None` so the type-checker
/// can still parse the file. The build aborts on `comptime_errors` before
/// emitting, so the placeholder value is never written to output.
fn comptime_fallback_line(line: &str) -> String {
    let indent_len = line
        .find(|c: char| !c.is_whitespace())
        .unwrap_or(line.len());
    let rest = &line[indent_len..];
    let indent = &line[..indent_len];
    let trail = if line.ends_with('\n') { "\n" } else { "" };

    // Strip `comptime val`/`comptime var`.
    let after_kw = rest
        .strip_prefix("comptime val ")
        .or_else(|| rest.strip_prefix("comptime var "))
        .unwrap_or(rest);

    // Find `NAME: TYPE` part (before the `=`).
    if let Some(eq) = find_assign_eq(after_kw) {
        let decl = &after_kw[..eq];
        return format!("{}{}= None{}", indent, decl, trail);
    }

    // Absolute fallback: emit a comment so it at least parses.
    format!("{}# comptime (evaluation failed){}", indent, trail)
}

// ── `T?` / `call()?` rewrites ─────────────────────────────────────────────────

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

/// Replace every `?` outside string literals and comments with the
/// appropriate substitution:
///
/// - `?` after an identifier char (`\w`, `_`, `]`) → ` | None`
///   (annotation nullable sugar `T?`).
/// - `?` after `)` → `.__typhon_try__()`
///   (Result-unwrap try-operator).
///
/// Returns the rewritten line and a list of 0-based byte columns in the
/// rewritten string where each ` | None` insertion *starts* (for the
/// formatter to restore `?`). Try-operator rewrites are NOT recorded in
/// the column list (the desugar pass handles them directly).
///
/// The `in_string` argument tracks triple-quoted string state across lines.
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
            // `call()?` — Result try-operator: `?` directly after `)`.
            if matches!(last_char, Some(')')) {
                out.push_str(".__typhon_try__()");
                last_char = Some(')');
                continue;
            }

            // `T?` — annotation nullable sugar: `?` after identifier char,
            // `_`, or `]`.
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

            // Unknown position — pass through; the parser will report a
            // syntax error.
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

// ── postprocess (restore Typhon syntax for fmt) ───────────────────────────────

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

// ── tests ─────────────────────────────────────────────────────────────────────

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
        assert!(result.python_source.contains("line two with ?"));
        assert!(result.optionals.is_empty());
    }

    #[test]
    fn handles_non_ascii_source_without_corruption() {
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

    // ── model keyword tests ────────────────────────────────────────────────────

    #[test]
    fn model_keyword_simple() {
        let result = preprocess("model User:\n    id: int\n");
        assert!(
            result.python_source.starts_with("class User(__TyphonModel__):"),
            "output: {:?}",
            result.python_source
        );
    }

    #[test]
    fn model_keyword_with_bases() {
        let result = preprocess("model ApiUser(BaseModel):\n    id: int\n");
        assert!(
            result
                .python_source
                .starts_with("class ApiUser(BaseModel, __TyphonModel__):"),
            "output: {:?}",
            result.python_source
        );
    }

    #[test]
    fn model_keyword_indented() {
        let src = "    model Inner:\n        x: int\n";
        let result = preprocess(src);
        assert!(
            result.python_source.starts_with("    class Inner(__TyphonModel__):"),
            "output: {:?}",
            result.python_source
        );
    }

    #[test]
    fn model_keyword_no_false_positive() {
        // `modelname = 1` should NOT be transformed.
        let result = preprocess("modelname = 1\n");
        assert_eq!(result.python_source, "modelname = 1\n");
    }

    // ── comptime keyword tests ─────────────────────────────────────────────────

    #[test]
    fn comptime_env_with_default_set() {
        // Set a dummy env var for this test.
        std::env::set_var("TYPHON_TEST_PORT", "9000");
        let result = preprocess("comptime val PORT: int = int(env(\"TYPHON_TEST_PORT\", \"8080\"))\n");
        std::env::remove_var("TYPHON_TEST_PORT");
        assert!(result.comptime_errors.is_empty(), "{:?}", result.comptime_errors);
        assert!(
            result.python_source.contains("PORT: int = 9000"),
            "output: {:?}",
            result.python_source
        );
    }

    #[test]
    fn comptime_env_uses_default_when_not_set() {
        std::env::remove_var("TYPHON_UNSET_VAR_XYZ");
        let result =
            preprocess("comptime val PORT: int = int(env(\"TYPHON_UNSET_VAR_XYZ\", \"8080\"))\n");
        assert!(result.comptime_errors.is_empty(), "{:?}", result.comptime_errors);
        assert!(
            result.python_source.contains("PORT: int = 8080"),
            "output: {:?}",
            result.python_source
        );
    }

    #[test]
    fn comptime_env_string_with_default() {
        std::env::remove_var("TYPHON_DB_URL_TEST");
        let result = preprocess(
            "comptime val DB_URL: str = env(\"TYPHON_DB_URL_TEST\", \"sqlite:///dev.db\")\n",
        );
        assert!(result.comptime_errors.is_empty(), "{:?}", result.comptime_errors);
        assert!(
            result.python_source.contains("sqlite:///dev.db"),
            "output: {:?}",
            result.python_source
        );
    }

    #[test]
    fn comptime_required_env_missing_produces_error() {
        std::env::remove_var("TYPHON_REQUIRED_TEST_VAR");
        let result =
            preprocess("comptime val SECRET: str = env(\"TYPHON_REQUIRED_TEST_VAR\")\n");
        assert!(
            !result.comptime_errors.is_empty(),
            "expected a comptime error for missing required env var"
        );
        assert!(
            result.comptime_errors[0].contains("TYPHON_REQUIRED_TEST_VAR"),
            "{:?}",
            result.comptime_errors
        );
    }

    // ── ? try-operator tests ────────────────────────────────────────────────────

    #[test]
    fn try_operator_after_call() {
        let result = preprocess("val x = db.find(id)?\n");
        // `?` follows `)` → try operator
        assert!(
            result.python_source.contains("db.find(id).__typhon_try__()"),
            "output: {:?}",
            result.python_source
        );
        // Must not generate a column mark (that is only for annotation `?`)
        assert!(result.optionals.is_empty(), "{:?}", result.optionals);
    }

    #[test]
    fn annotation_question_mark_unchanged_after_try_operator_change() {
        // `str?` in annotation context must still become `str | None`.
        let result = preprocess("val email: str? = None\n");
        assert_eq!(result.python_source, "email: str | None = None\n");
    }

    #[test]
    fn try_operator_in_return() {
        let result = preprocess("    return lookup(key)?\n");
        assert!(
            result.python_source.contains("lookup(key).__typhon_try__()"),
            "output: {:?}",
            result.python_source
        );
    }

    #[test]
    fn try_operator_not_in_string() {
        let result = preprocess("s = \"call()?\"\n");
        assert!(!result.python_source.contains("__typhon_try__"), "output: {:?}", result.python_source);
    }
}
