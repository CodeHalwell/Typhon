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

            // ── `impl ClassName:` → `class __typhon_impl_ClassName(object):` ─
            if rest.starts_with("impl ")
                && rest.len() > "impl ".len()
                && (rest.as_bytes()["impl ".len()].is_ascii_alphanumeric()
                    || rest.as_bytes()["impl ".len()] == b'_')
            {
                let after_impl = &rest["impl ".len()..];
                if let Some(class_header) = make_impl_class_line(after_impl) {
                    stripped.push(StrippedKeyword {
                        line_index,
                        keyword: TyphonKeyword::Impl,
                    });
                    let new_line = format!("{}class {}", indent, class_header);
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
                    if after_comptime.starts_with("val ") && after_comptime.len() > 4 {
                        (Some(TyphonKeyword::Val), &after_comptime["val ".len()..])
                    } else if after_comptime.starts_with("var ") && after_comptime.len() > 4 {
                        (Some(TyphonKeyword::Var), &after_comptime["var ".len()..])
                    } else {
                        (None, after_comptime)
                    };

                // Extract the binding name (first identifier before `:` or `=`).
                let binding_name = payload
                    .split([':', '='])
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

/// Build the class-header portion of an `impl` line, converting
/// `ClassName:\n` into `__typhon_impl_ClassName(object):\n`.
///
/// `impl` blocks do not accept base-class lists; any `(...)` suffix on the
/// class name is stripped rather than forwarded, preventing a Python syntax
/// error from a doubly-parenthesised expression like
/// `class __typhon_impl_User(Base)(object):`.
///
/// Returns `None` when the line doesn't look like a class header (no `:`).
fn make_impl_class_line(after_impl: &str) -> Option<String> {
    let mut depth = 0i32;
    let mut colon_pos = None;
    for (i, c) in after_impl.char_indices() {
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
    let raw = after_impl[..colon_pos].trim_end();
    // Strip any base-class list — `impl` blocks don't support inheritance.
    let name = if let Some(paren) = raw.find('(') {
        raw[..paren].trim_end()
    } else {
        raw
    };
    let tail = &after_impl[colon_pos..]; // ":\n" or ":"
    Some(format!("__typhon_impl_{}(object){}", name, tail))
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
    let new_name = if let Some(stripped) = name.strip_suffix(')') {
        format!("{stripped}, BaseModel)")
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
fn rewrite_optionals(line: &str, in_string: &mut Option<StringMode>) -> (String, Vec<usize>) {
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
    if matches!(
        *in_string,
        Some(StringMode::Single) | Some(StringMode::Double)
    ) {
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
        per_line
            .entry(opt.line_index)
            .or_default()
            .push(opt.python_col);
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
    let mut insertions: Vec<(usize, TyphonKeyword)> = stripped
        .iter()
        .map(|sk| (sk.line_index, sk.keyword))
        .collect();
    // Sort descending by line_index; for identical line_index values preserve
    // original order (stable sort) so that `val` is restored before `comptime`
    // on the same line, allowing `comptime` to be prepended on top.
    insertions.sort_by(|a, b| b.0.cmp(&a.0));
    for (line_idx, kw) in insertions {
        if line_idx >= lines.len() {
            continue;
        }
        match kw {
            TyphonKeyword::Impl => {
                // Restore `class __typhon_impl_X(object):` → `impl X:`.
                let line = &lines[line_idx];
                let indent_len = line
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(line.len());
                let content = &line[indent_len..];
                let restored = if let Some(tail) = content.strip_prefix("class __typhon_impl_") {
                    let tail = tail.replacen("(object)", "", 1);
                    format!("impl {}", tail)
                } else {
                    format!("impl {}", content)
                };
                lines[line_idx] = format!("{}{}", &line[..indent_len], restored);
            }
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

// ── `?` operator validation ───────────────────────────────────────────────────

/// An error produced when the `?` operator is used in an invalid context.
#[derive(Debug, Clone)]
pub struct QuestionOpError {
    /// 0-based line index in the original Typhon source.
    pub line_index: usize,
    /// Byte offset of the `?` character in the original source.
    pub offset: usize,
    /// Human-readable error message.
    pub message: String,
}

/// Validate that every `?` error-propagation operator (`expr)?`) in `source`
/// appears inside a function whose return-type annotation is `Result[T, E]`.
///
/// Returns a list of errors for each `)?` found at module level or inside a
/// function whose return-type annotation is known and does not begin with the
/// `Result` identifier.
///
/// Multi-line signatures where `->` appears on the `) -> RetType:` closing
/// line (rather than the `def` line itself) are handled: the validator detects
/// the `) -> RetType:` pattern and records the return type.  Signatures where
/// `->` appears on neither the `def` line nor the closing `)` line are treated
/// as having an unknown return type and the `?` check is skipped for them to
/// avoid false positives.
///
/// Call this on the **original Typhon source** before [`expand_question_ops`].
pub fn validate_question_ops(source: &str) -> Vec<QuestionOpError> {
    let mut errors = Vec::new();
    // Stack of (def_indent_len, return_type_text):
    //   def_indent_len   — indentation column of the `def` keyword itself.
    //   return_type_text — Some(text) when `->` was found; None means unknown.
    let mut fn_stack: Vec<(usize, Option<String>)> = Vec::new();
    let mut in_string: Option<StringMode> = None;
    // Running byte offset of the start of the current line in `source`.
    let mut byte_offset: usize = 0;

    for (line_index, line) in source.split_inclusive('\n').enumerate() {
        let raw = line.trim_end_matches(['\n', '\r']);

        let pre_string = in_string;
        let code_end = scan_line_code_end(raw, &mut in_string);

        // Lines inside a triple-quoted string are pure string content — skip.
        if pre_string.is_some() {
            byte_offset += line.len();
            continue;
        }

        let code = raw[..code_end].trim_end();

        // Blank and comment-only lines don't affect scope tracking.
        if code.trim().is_empty() {
            byte_offset += line.len();
            continue;
        }

        let indent_len = code.find(|c: char| !c.is_whitespace()).unwrap_or(0);
        let trimmed = &code[indent_len..];

        // A line that begins with `)` is the continuation/close of a multi-line
        // expression — most commonly `) -> RetType:` closing a multi-line
        // parameter list.  Suppress scope-pop for these lines; if `->` is
        // present, update the most-recent function's return type.  Unlike the
        // previous approach, we do NOT skip the `?` detection below so that
        // `)?` on the same line as a closing paren is still validated.
        if trimmed.starts_with(')') {
            if trimmed.contains("->") {
                if let Some(entry) = fn_stack.last_mut() {
                    if entry.1.is_none() {
                        entry.1 = extract_return_type_text(code);
                    }
                }
            }
        } else {
            // Pop functions we've exited: a non-blank line at indent ≤ fn_indent
            // means we have left that function's body.
            while let Some(&(fn_indent, _)) = fn_stack.last() {
                if indent_len <= fn_indent {
                    fn_stack.pop();
                } else {
                    break;
                }
            }

            // Detect a function definition and push its return type onto the stack.
            if trimmed.starts_with("def ") || trimmed.starts_with("async def ") {
                let ret_type = extract_return_type_text(code);
                fn_stack.push((indent_len, ret_type));
            }
        }

        // Detect `)?` — the `?` error-propagation operator.  The same pattern
        // `expand_question_ops` uses: last code char is `?`, char before is `)`.
        // This check runs for ALL lines, including `)…` continuation lines.
        if let Some(before_q) = code.strip_suffix('?') {
            if before_q.ends_with(')') {
                // Byte offset of the `?` in the original source.
                let q_offset = byte_offset + code.len() - 1;
                match fn_stack.last() {
                    None => {
                        errors.push(QuestionOpError {
                            line_index,
                            offset: q_offset,
                            message: "`?` operator used at module level; \
                                     it is only valid inside a function returning `Result[T, E]`"
                                .to_owned(),
                        });
                    }
                    Some((_, Some(ret))) if !is_result_type(ret) => {
                        errors.push(QuestionOpError {
                            line_index,
                            offset: q_offset,
                            message: format!(
                                "`?` operator used in a function returning `{ret}`; \
                                 it is only valid in functions returning `Result[T, E]`"
                            ),
                        });
                    }
                    // Return type is Result-family (valid) or unknown (skip FP).
                    Some(_) => {}
                }
            }
        }

        byte_offset += line.len();
    }

    errors
}

/// Return `true` when `ret` is the `Result` identifier (bare or subscripted).
///
/// Checks for a whole-word match so that `MyResult` / `NotAResult` are not
/// accepted.  The canonical Typhon form is `Result[T, E]`.
fn is_result_type(ret: &str) -> bool {
    let ret = ret.trim();
    if !ret.starts_with("Result") {
        return false;
    }
    // The character after "Result" (if any) must not be an identifier char.
    matches!(
        ret.as_bytes().get("Result".len()),
        None | Some(b'[') | Some(b' ') | Some(b'\t')
    )
}

/// Extract the return-type annotation text from a single `def` line.
///
/// Returns `Some(text)` when `->` appears outside string literals,
/// parentheses, and brackets on this line and the header-closing `:` is also
/// on the same line; `None` otherwise (multi-line signatures, missing
/// annotation, etc.).
///
/// String literal tracking prevents `->` or `:` inside a default-value string
/// (e.g. `def f(x: str = "->") -> Result:`) from confusing the parser.
fn extract_return_type_text(def_line: &str) -> Option<String> {
    let bytes = def_line.as_bytes();
    let mut depth = 0i32;
    let mut in_str: Option<u8> = None; // Some(b'"') or Some(b'\'')
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        // Inside a single-/double-quoted string: look for the closing quote,
        // handling backslash escapes.  (Triple-quoted strings are unlikely in
        // a `def` parameter list and are not handled here.)
        if let Some(q) = in_str {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' => in_str = Some(b),
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth = (depth - 1).max(0),
            b'-' if depth == 0 && i + 1 < bytes.len() && bytes[i + 1] == b'>' => {
                let after_arrow = def_line[i + 2..].trim_start();
                // Scan the return-type text for the colon that closes the
                // function header, respecting nested brackets and strings.
                let mut depth2 = 0i32;
                let mut in_str2: Option<u8> = None;
                let mut colon_pos = None;
                let abytes = after_arrow.as_bytes();
                let mut j = 0;
                while j < abytes.len() {
                    let c = abytes[j];
                    if let Some(q) = in_str2 {
                        if c == b'\\' {
                            j += 2;
                            continue;
                        }
                        if c == q {
                            in_str2 = None;
                        }
                        j += 1;
                        continue;
                    }
                    match c {
                        b'"' | b'\'' => in_str2 = Some(c),
                        b'(' | b'[' => depth2 += 1,
                        b')' | b']' => depth2 -= 1,
                        b':' if depth2 == 0 => {
                            colon_pos = Some(j);
                            break;
                        }
                        _ => {}
                    }
                    j += 1;
                }
                return colon_pos.map(|pos| after_arrow[..pos].trim().to_owned());
            }
            _ => {}
        }
        i += 1;
    }
    None
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
        let raw = line.trim_end_matches(['\n', '\r']);

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
    let chars = s.char_indices().peekable();

    for (i, c) in chars {
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
                if !matches!(prev, Some('!' | '<' | '>' | '=')) && !matches!(next, Some('=')) {
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
        assert!(result
            .stripped
            .iter()
            .any(|k| matches!(k.keyword, TyphonKeyword::Model)));
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
        assert!(
            result.python_source.contains("class Inner(BaseModel):"),
            "output: {}",
            result.python_source
        );
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
        assert_eq!(
            result.python_source,
            "DB_URL: str = \"postgres://localhost\"\n"
        );
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
        assert!(
            out.contains("if isinstance(__typhon_q_0__, Err):"),
            "out: {out}"
        );
        assert!(out.contains("return __typhon_q_0__"), "out: {out}");
        assert!(out.contains("val x = __typhon_q_0__.value"), "out: {out}");
    }

    #[test]
    fn question_op_expands_bare_call() {
        let src = "save(record)?\n";
        let out = expand_question_ops(src);
        assert!(out.contains("__typhon_q_0__ = save(record)"), "out: {out}");
        assert!(
            out.contains("if isinstance(__typhon_q_0__, Err):"),
            "out: {out}"
        );
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
        assert!(
            out.contains("    val y = __typhon_q_0__.value"),
            "out: {out}"
        );
    }

    #[test]
    fn question_op_preserves_lhs_with_type_annotation() {
        let src = "val result: int = compute()?\n";
        let out = expand_question_ops(src);
        assert!(
            out.contains("val result: int = __typhon_q_0__.value"),
            "out: {out}"
        );
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
        assert_eq!(
            out, src,
            "trailing comment with ')?' must not trigger expansion"
        );
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
            result
                .python_source
                .contains("class User(Timestamped, BaseModel):"),
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

    // ── impl keyword ─────────────────────────────────────────────────────────

    #[test]
    fn impl_keyword_becomes_typhon_impl_class() {
        let result = preprocess("impl User:\n    def greet():\n        pass\n");
        assert!(
            result
                .python_source
                .contains("class __typhon_impl_User(object):"),
            "output: {}",
            result.python_source
        );
        assert!(result
            .stripped
            .iter()
            .any(|k| matches!(k.keyword, TyphonKeyword::Impl)));
    }

    #[test]
    fn impl_keyword_round_trips_via_postprocess() {
        let src = "impl User:\n    def greet():\n        pass\n";
        let prep = preprocess(src);
        let out = postprocess(&prep.python_source, &prep.stripped, &prep.optionals);
        assert_eq!(out, src);
    }

    #[test]
    fn indented_impl_preserved() {
        let src = "    impl Inner:\n        def method():\n            pass\n";
        let result = preprocess(src);
        assert!(
            result
                .python_source
                .contains("class __typhon_impl_Inner(object):"),
            "output: {}",
            result.python_source
        );
    }

    #[test]
    fn impl_underscore_name() {
        let result = preprocess("impl _Private:\n    def method():\n        pass\n");
        assert!(
            result
                .python_source
                .contains("class __typhon_impl__Private(object):"),
            "output: {}",
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

    // ── validate_question_ops ────────────────────────────────────────────────

    #[test]
    fn question_op_valid_in_result_function() {
        let src = "def parse(s: str) -> Result[int, str]:\n    val n = int(s)?\n    return Ok(n)\n";
        let errs = validate_question_ops(src);
        assert!(
            errs.is_empty(),
            "expected no errors, got: {:?}",
            errs.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn question_op_at_module_level_is_error() {
        let src = "val x = parse()?\n";
        let errs = validate_question_ops(src);
        assert_eq!(errs.len(), 1);
        assert!(
            errs[0].message.contains("module level"),
            "got: {}",
            errs[0].message
        );
        assert_eq!(errs[0].line_index, 0);
    }

    #[test]
    fn question_op_in_none_returning_function_is_error() {
        let src = "def process() -> None:\n    val x = load()?\n";
        let errs = validate_question_ops(src);
        assert_eq!(
            errs.len(),
            1,
            "expected one error, got: {:?}",
            errs.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        assert!(errs[0].message.contains("None"), "got: {}", errs[0].message);
    }

    #[test]
    fn question_op_in_int_returning_function_is_error() {
        let src = "def compute() -> int:\n    val x = fetch()?\n    return x\n";
        let errs = validate_question_ops(src);
        assert_eq!(errs.len(), 1);
        assert!(
            errs[0].message.contains("`int`"),
            "got: {}",
            errs[0].message
        );
    }

    #[test]
    fn question_op_in_nested_result_function_is_valid() {
        let src = "def outer() -> None:\n    def inner() -> Result[int, str]:\n        val x = load()?\n        return Ok(x)\n";
        let errs = validate_question_ops(src);
        assert!(
            errs.is_empty(),
            "expected no errors, got: {:?}",
            errs.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn question_op_in_multiline_sig_skips_validation() {
        // Multi-line signature: `->` appears on the `) -> RetType:` closing
        // line rather than the `def` line.  The validator picks up the return
        // type from the signature-closer and correctly accepts the `?` use.
        let src = "def process(\n    x: int,\n) -> Result[int, str]:\n    val y = load()?\n";
        let errs = validate_question_ops(src);
        assert!(
            errs.is_empty(),
            "multi-line sig with Result return must not error: {:?}",
            errs.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn question_op_nullable_sugar_not_flagged() {
        // `str?` ends with `?` but the char before is `r`, not `)`, so it is
        // type-sugar, not the propagation operator.
        let src = "val x: str? = None\n";
        let errs = validate_question_ops(src);
        assert!(errs.is_empty(), "nullable sugar must not be flagged");
    }

    #[test]
    fn extract_return_type_finds_result() {
        assert_eq!(
            extract_return_type_text("def f() -> Result[int, str]:"),
            Some("Result[int, str]".to_owned())
        );
    }

    #[test]
    fn extract_return_type_finds_none() {
        assert_eq!(
            extract_return_type_text("def f() -> None:"),
            Some("None".to_owned())
        );
    }

    #[test]
    fn extract_return_type_multiline_sig() {
        // `->` not present on this line — returns None.
        assert_eq!(extract_return_type_text("def f("), None);
    }

    #[test]
    fn extract_return_type_ignores_arrow_in_string() {
        // `->` inside a default-value string must not fool the scanner.
        assert_eq!(
            extract_return_type_text("def f(x: str = \"->\") -> Result[int, str]:"),
            Some("Result[int, str]".to_owned())
        );
    }

    #[test]
    fn extract_return_type_ignores_colon_in_string() {
        assert_eq!(
            extract_return_type_text("def f(x: str = \"a:b\") -> None:"),
            Some("None".to_owned())
        );
    }

    #[test]
    fn is_result_type_accepts_bare_result() {
        assert!(is_result_type("Result"));
    }

    #[test]
    fn is_result_type_accepts_subscripted_result() {
        assert!(is_result_type("Result[int, str]"));
    }

    #[test]
    fn is_result_type_rejects_substring() {
        assert!(!is_result_type("MyResult"));
        assert!(!is_result_type("ResultWrapper[int]"));
        assert!(!is_result_type("NotAResult"));
    }

    #[test]
    fn question_op_my_result_type_is_invalid_context() {
        // `MyResult` must not be accepted as a valid Result context.
        let src = "def f() -> MyResult:\n    val x = load()?\n";
        let errs = validate_question_ops(src);
        assert_eq!(errs.len(), 1, "MyResult must not pass as Result context");
        assert!(
            errs[0].message.contains("MyResult"),
            "got: {}",
            errs[0].message
        );
    }

    #[test]
    fn question_op_error_carries_byte_offset() {
        // "val x = load()?" — v(0)a(1)l(2) (3)x(4) (5)=(6) (7)l(8)o(9)a(10)d(11)((12))(13)?(14)
        let src = "val x = load()?\n";
        let errs = validate_question_ops(src);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].offset, 14, "offset should point at the `?`");
    }
}
