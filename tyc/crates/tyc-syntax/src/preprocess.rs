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

/// A `lazy import` declaration — `lazy import ALIAS = MODULE`.
///
/// At check/format time the preprocessor converts this to `import MODULE as
/// ALIAS` so the rest of the pipeline sees a standard Python import.  At build
/// time [`expand_lazy_imports`] replaces it with a thread-safe, on-first-use
/// loader before the main preprocess pass runs.
#[derive(Debug, Clone)]
pub struct LazyImport {
    /// 0-based line index in both the original and preprocessed source.
    pub line_index: usize,
    /// The local alias (`np` in `lazy import np = numpy`).
    pub alias: String,
    /// The module being imported (`numpy` in `lazy import np = numpy`).
    pub module: String,
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
    /// Lazy import declarations, in source order.  Each entry records the
    /// alias and module so [`postprocess`] can restore the original
    /// `lazy import ALIAS = MODULE` syntax.
    pub lazy_imports: Vec<LazyImport>,
}

/// Strip Typhon-specific syntax from `source` and return the Python-
/// compatible string together with restoration metadata.
pub fn preprocess(source: &str) -> PreprocessResult {
    let mut python_source = String::with_capacity(source.len());
    let mut stripped = Vec::new();
    let mut optionals = Vec::new();
    let mut comptime_bindings = Vec::new();
    let mut lazy_imports = Vec::new();
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

            // ── `lazy import ALIAS = MODULE` → `import MODULE as ALIAS` ─────
            // Only recognised at module level (indent_len == 0) so that
            // indented `lazy` expressions (rare but valid Python identifiers)
            // are not mistakenly rewritten.
            if indent_len == 0 {
                if let Some(after_raw) = rest.strip_prefix("lazy import ") {
                    let after = after_raw.trim_end_matches(['\n', '\r']);
                    if let Some((alias, module)) = parse_lazy_import(after) {
                        lazy_imports.push(LazyImport {
                            line_index,
                            alias: alias.clone(),
                            module: module.clone(),
                        });
                        // Emit a standard Python import so downstream passes see a
                        // valid import statement.
                        let new_line = format!("import {} as {}\n", module, alias);
                        python_source.push_str(&new_line);
                        continue;
                    }
                    // Unrecognised `lazy import` form — fall through to produce a
                    // parse error from the Python parser.
                }
            }

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
        lazy_imports,
    }
}

/// Parse the tail of a `lazy import` line: `ALIAS = MODULE`.
///
/// Returns `(alias, module)` on success, `None` if the syntax is malformed.
fn parse_lazy_import(tail: &str) -> Option<(String, String)> {
    // Strip a trailing `# comment` so that `lazy import np = numpy  # noqa`
    // is handled correctly rather than failing the identifier check.
    let code = tail.split('#').next().unwrap_or("").trim();
    let eq = code.find('=')?;
    let alias = code[..eq].trim().to_owned();
    let module = code[eq + 1..].trim().to_owned();
    if !is_python_ident(&alias) || !is_dotted_python_ident(&module) {
        return None;
    }
    Some((alias, module))
}

/// True iff `s` is a valid Python identifier (starts with alpha or `_`,
/// followed by alphanumerics or `_`).
fn is_python_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// True iff `s` is a dotted Python module path (each component is a valid
/// Python identifier, e.g. `numpy` or `numpy.random`).
fn is_dotted_python_ident(s: &str) -> bool {
    !s.is_empty()
        && s.split('.')
            .all(|part| !part.is_empty() && is_python_ident(part))
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
        // If the class had an empty base list `()`, stripped ends with `(`
        // and we must not emit the separating `, `.
        let trimmed = stripped.trim_end();
        if trimmed.ends_with('(') {
            format!("{trimmed}BaseModel)")
        } else {
            format!("{trimmed}, BaseModel)")
        }
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

/// Restore stripped keywords, `?` sugar, and `lazy import` declarations into
/// a normalised Python source string.
///
/// `normalised` is the Python source after whitespace normalisation.
pub fn postprocess(
    normalised: &str,
    stripped: &[StrippedKeyword],
    optionals: &[StrippedOptional],
) -> String {
    postprocess_full(normalised, stripped, optionals, &[])
}

/// Like [`postprocess`] but also restores `lazy import` lines.
pub fn postprocess_full(
    normalised: &str,
    stripped: &[StrippedKeyword],
    optionals: &[StrippedOptional],
    lazy_imports: &[LazyImport],
) -> String {
    if stripped.is_empty() && optionals.is_empty() && lazy_imports.is_empty() {
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
            // `lazy import` restoration is handled separately below via
            // `lazy_imports`; the `Lazy` keyword is never pushed into
            // `stripped`, so this arm is a safety no-op.
            TyphonKeyword::Lazy => {}
        }
    }

    // Restore `lazy import ALIAS = MODULE` lines.  The preprocessor emitted
    // `import MODULE as ALIAS` in their place; replace that with the original
    // Typhon syntax.
    for li in lazy_imports {
        if li.line_index >= lines.len() {
            continue;
        }
        lines[li.line_index] = format!("lazy import {} = {}", li.alias, li.module);
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

/// Expand `lazy import ALIAS = MODULE` declarations into thread-safe,
/// on-first-access loader code.
///
/// Each `lazy import` line at module level (indent = 0) is replaced by a
/// class-based proxy that imports the module on the first attribute access:
///
/// ```text
/// # Input (Typhon)
/// lazy import np = numpy
///
/// # Output (valid Python)
/// class __TyphonLazy_np_:
///     __slots__ = ('_m', '_lock')
///     def __init__(self):
///         import threading as _t   # local import avoids __future__ conflicts
///         object.__setattr__(self, '_m', None)
///         object.__setattr__(self, '_lock', _t.Lock())
///     def __getattr__(self, name):
///         m = object.__getattribute__(self, '_m')
///         if m is None:
///             lock = object.__getattribute__(self, '_lock')
///             with lock:
///                 m = object.__getattribute__(self, '_m')
///                 if m is None:
///                     import numpy as _mod
///                     object.__setattr__(self, '_m', _mod)
///                     m = _mod
///         return getattr(m, name)
///     def __repr__(self):
///         m = object.__getattribute__(self, '_m')
///         return repr(m) if m is not None else '<lazy module numpy>'
/// np = __TyphonLazy_np_()
/// ```
///
/// `lazy from x import a, b` is not supported and is left unchanged so that
/// the Python parser produces a diagnostic at the offending line.
///
/// This function is called **before** [`preprocess`] in the build pipeline.
/// It is deliberately *not* called by `tyc fmt` or the check pipeline (which
/// use [`preprocess`]'s simpler `import MODULE as ALIAS` conversion instead).
pub fn expand_lazy_imports(source: &str) -> String {
    let mut result = String::with_capacity(source.len() + 256);
    // Track triple-quoted string state so that a `lazy import` that appears
    // inside a docstring or multiline string is never mistakenly rewritten.
    let mut in_string: Option<StringMode> = None;

    for line in source.split_inclusive('\n') {
        let pre_string = in_string;
        let raw = line.trim_end_matches(['\n', '\r']);
        let _code_end = scan_line_code_end(raw, &mut in_string);

        // Lines that begin inside a triple-quoted string are pure string
        // content — emit verbatim.
        if pre_string.is_some() {
            result.push_str(line);
            continue;
        }

        let trimmed = raw.trim_start();
        let indent_len = raw.len() - trimmed.len();

        // Only expand at module level (indent_len == 0).
        if indent_len == 0 {
            if let Some(after) = trimmed.strip_prefix("lazy import ") {
                if let Some((alias, module)) = parse_lazy_import(after) {
                    emit_lazy_proxy(&mut result, &alias, &module);
                    continue;
                }
            }
        }

        result.push_str(line);
    }

    result
}

/// Emit the proxy class for a single `lazy import ALIAS = MODULE`.
///
/// The `threading` import is placed inside `__init__` rather than at module
/// level so that it cannot conflict with `from __future__ import ...` or
/// encoding cookies that must appear at the start of the file.
fn emit_lazy_proxy(out: &mut String, alias: &str, module: &str) {
    let class = format!("__TyphonLazy_{alias}_");
    out.push_str(&format!("class {class}:\n"));
    out.push_str("    __slots__ = ('_m', '_lock')\n");
    out.push_str("    def __init__(self):\n");
    // Import threading locally so the proxy carries no module-level side
    // effects and remains safe even when the source file has a __future__
    // prologue or a custom encoding cookie.
    out.push_str("        import threading as _t\n");
    out.push_str("        object.__setattr__(self, '_m', None)\n");
    out.push_str("        object.__setattr__(self, '_lock', _t.Lock())\n");
    out.push_str("    def __getattr__(self, name):\n");
    out.push_str("        m = object.__getattribute__(self, '_m')\n");
    out.push_str("        if m is None:\n");
    out.push_str("            lock = object.__getattribute__(self, '_lock')\n");
    out.push_str("            with lock:\n");
    out.push_str("                m = object.__getattribute__(self, '_m')\n");
    out.push_str("                if m is None:\n");
    out.push_str(&format!("                    import {module} as _mod\n"));
    out.push_str("                    object.__setattr__(self, '_m', _mod)\n");
    out.push_str("                    m = _mod\n");
    out.push_str("        return getattr(m, name)\n");
    out.push_str("    def __dir__(self):\n");
    out.push_str("        m = object.__getattribute__(self, '_m')\n");
    out.push_str("        return dir(m) if m is not None else []\n");
    out.push_str("    def __repr__(self):\n");
    out.push_str("        m = object.__getattribute__(self, '_m')\n");
    out.push_str(&format!(
        "        return repr(m) if m is not None else '<lazy module {module}>'\n"
    ));
    out.push_str(&format!("{alias} = {class}()\n"));
}

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

// ── `with`-chain expansion ────────────────────────────────────────────────────

/// Expand a `with`-chain into a flat sequence of guarded `Result` unwraps.
///
/// Source form:
///
/// ```text
/// with user   = db.find_user(id)?,
///      perms  = check_perms(user)?,
///      report = build_report(user, perms)?:
///     return Ok(report)
/// else err:
///     log.warn(err)
///     return Err(err)
/// ```
///
/// Each binding evaluates its RHS, returns the value when `Ok`, and runs the
/// `else` block (with `err` bound to the unwrapped error value) on the first
/// `Err`. When no `else` clause is provided the chain falls back to the
/// default propagation form — `return <tmp>` — matching the `?` operator.
///
/// This rewrite runs **before** [`expand_question_ops`] and [`expand_pipes`]
/// so that the rest of the pipeline sees only plain Python.
///
/// # Limitations
///
/// - The `with`-chain must sit at the top of its line and not be nested
///   inside another expression.
/// - Bindings continue on subsequent lines only; nesting another control
///   structure between bindings is not supported.
/// - The `else` body must be indented strictly deeper than the `with`
///   header. A bare `else:` (no binding name) defaults the error variable
///   name to `_err`.
pub fn expand_with_chains(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut counter: usize = 0;
    let mut in_string: Option<StringMode> = None;
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let pre_string = in_string;
        let raw = line.trim_end_matches(['\n', '\r']);
        let _code_end = scan_line_code_end(raw, &mut in_string);

        // Lines that start inside a triple-quoted string are pure content.
        if pre_string.is_some() {
            out.push_str(line);
            i += 1;
            continue;
        }

        // Look for a `with`-chain opener on this line.
        let indent_len = raw.find(|c: char| !c.is_whitespace()).unwrap_or(raw.len());
        let chain_indent = &raw[..indent_len];
        let body = &raw[indent_len..];

        if let Some(rest) = body.strip_prefix("with ") {
            if let Some((first_binding, first_term)) = parse_with_binding(rest) {
                // Re-scan the binding line's string state so the continuation
                // scanner picks up unfinished triple-quoted strings correctly.
                let mut state_for_chain = pre_string;
                let _ = scan_line_code_end(raw, &mut state_for_chain);

                if let Some((chain, consumed, end_state)) = collect_chain(
                    &lines,
                    i,
                    chain_indent,
                    first_binding,
                    first_term,
                    state_for_chain,
                ) {
                    let rendered = render_chain(&chain, chain_indent, &mut counter);
                    out.push_str(&rendered);
                    in_string = end_state;
                    i += consumed;
                    continue;
                }
            }
        }

        out.push_str(line);
        i += 1;
    }

    out
}

/// One unwrap step inside a `with`-chain.
#[derive(Debug)]
struct WithBinding {
    /// The variable name on the left-hand side of `=`.
    target: String,
    /// The Result-typed expression on the right (without the trailing `?`).
    expr: String,
}

#[derive(Debug)]
struct WithChain {
    bindings: Vec<WithBinding>,
    /// Body lines that run after every binding has unwrapped to its `Ok` value.
    /// Each entry is a verbatim line including its trailing newline.
    body: Vec<String>,
    /// The error-binding identifier supplied in `else <name>:`, or `None` for
    /// a default-propagating chain. A bare `else:` records `Some("_err")`.
    err_var: Option<String>,
    /// Body lines for the else branch, with the chain's base indentation
    /// removed (so each line starts at indent zero relative to the chain).
    else_body: Vec<String>,
}

/// Parse one `name = expr?(,|:)` segment from the tail of a binding line.
///
/// Returns the binding plus the terminator character (`,` or `:`).
fn parse_with_binding(s: &str) -> Option<(WithBinding, char)> {
    let s = s.trim_end();
    let term = if s.ends_with("?,") {
        ','
    } else if s.ends_with("?:") {
        ':'
    } else {
        return None;
    };
    let head = &s[..s.len() - 2];
    // Split on the first `=` at depth 0.
    let eq = find_assignment_eq(head)?;
    let target = head[..eq].trim();
    let expr = head[eq + 1..].trim();
    if target.is_empty() || expr.is_empty() {
        return None;
    }
    Some((
        WithBinding {
            target: target.to_owned(),
            expr: expr.to_owned(),
        },
        term,
    ))
}

/// Collect a `with`-chain that starts at `lines[start]`. Returns the chain,
/// the number of consumed lines, and the resulting triple-quoted-string state.
///
/// Returns `None` (and consumes no lines) if the construct is malformed —
/// the caller will then emit the source unchanged so the underlying parser
/// can surface the syntax error with full context.
fn collect_chain(
    lines: &[&str],
    start: usize,
    chain_indent: &str,
    first_binding: WithBinding,
    first_term: char,
    initial_string_state: Option<StringMode>,
) -> Option<(WithChain, usize, Option<StringMode>)> {
    let mut bindings = vec![first_binding];
    let mut idx = start + 1;
    let mut term = first_term;
    let mut in_string = initial_string_state;

    // Collect continuation binding lines while the previous terminator was `,`.
    while term == ',' && idx < lines.len() {
        let line = lines[idx];
        let raw = line.trim_end_matches(['\n', '\r']);
        let _ = scan_line_code_end(raw, &mut in_string);

        let indent_len = raw.find(|c: char| !c.is_whitespace()).unwrap_or(raw.len());
        let line_indent = &raw[..indent_len];
        // Continuation lines must be indented strictly past the `with`.
        if line_indent.len() <= chain_indent.len() {
            return None;
        }
        let body = &raw[indent_len..];
        let (binding, t) = parse_with_binding(body)?;
        bindings.push(binding);
        term = t;
        idx += 1;
    }
    // The terminating `:` must have been seen by now.
    if term != ':' {
        return None;
    }

    // The success body: every line whose indent exceeds the chain's. Strip the
    // chain indent from each line so the caller can re-indent uniformly.
    let mut body = Vec::new();
    while idx < lines.len() {
        let line = lines[idx];
        let raw = line.trim_end_matches(['\n', '\r']);
        if raw.trim().is_empty() {
            // Pass blank lines through verbatim.
            let _ = scan_line_code_end(raw, &mut in_string);
            body.push(line.to_string());
            idx += 1;
            continue;
        }
        let indent_len = raw.find(|c: char| !c.is_whitespace()).unwrap_or(raw.len());
        if indent_len <= chain_indent.len() {
            break;
        }
        let _ = scan_line_code_end(raw, &mut in_string);
        body.push(line.to_string());
        idx += 1;
    }
    if body.is_empty() {
        return None;
    }

    // Optional `else <name>:` continuation at the chain indent. The header
    // must end with `:`; a malformed `else err` (no colon) is *not* silently
    // accepted — the whole chain is rejected and emitted verbatim so the
    // underlying Python parser surfaces a syntax error pointing at the
    // mistake.
    let mut err_var = None;
    let mut else_body = Vec::new();
    if idx < lines.len() {
        let line = lines[idx];
        let raw = line.trim_end_matches(['\n', '\r']);
        let indent_len = raw.find(|c: char| !c.is_whitespace()).unwrap_or(raw.len());
        let header = raw[indent_len..].trim_end();
        let is_else_header =
            indent_len == chain_indent.len() && (header == "else:" || header.starts_with("else "));
        if is_else_header {
            // Require a trailing colon on the header. `else err` (no colon)
            // is rejected so the user sees a Python-level syntax error.
            if !header.ends_with(':') {
                return None;
            }
            err_var = Some(if header == "else:" {
                "_err".to_owned()
            } else {
                parse_else_var(header)?
            });
            let _ = scan_line_code_end(raw, &mut in_string);
            idx += 1;
            while idx < lines.len() {
                let l = lines[idx];
                let r = l.trim_end_matches(['\n', '\r']);
                if r.trim().is_empty() {
                    let _ = scan_line_code_end(r, &mut in_string);
                    else_body.push(l.to_string());
                    idx += 1;
                    continue;
                }
                let ind = r.find(|c: char| !c.is_whitespace()).unwrap_or(r.len());
                if ind <= chain_indent.len() {
                    break;
                }
                let _ = scan_line_code_end(r, &mut in_string);
                else_body.push(l.to_string());
                idx += 1;
            }
            if else_body.is_empty() {
                return None;
            }
        }
    }

    let consumed = idx - start;
    Some((
        WithChain {
            bindings,
            body,
            err_var,
            else_body,
        },
        consumed,
        in_string,
    ))
}

fn parse_else_var(header: &str) -> Option<String> {
    let rest = header.strip_prefix("else")?.trim_start();
    if let Some(name) = rest.strip_suffix(':') {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        if !name
            .chars()
            .next()
            .map(|c| c.is_ascii_alphabetic() || c == '_')
            .unwrap_or(false)
        {
            return None;
        }
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        Some(name.to_owned())
    } else {
        None
    }
}

/// Render a `with`-chain back into Python. The output is a flat sequence of
/// guarded `if isinstance(tmp, Err): ...` checks followed by the success body.
///
/// The success body lines originated one indent level inside the `with`
/// header; after flattening they become siblings of the unwrap statements and
/// must shed exactly that level. Else-body lines stay inside the new `if`
/// block, so their indent relative to the chain is preserved by emitting
/// them verbatim. The unit-indent is detected from the source itself (the
/// first non-blank body line) rather than assumed to be four spaces, so
/// tab-indented or two-space code round-trips correctly.
fn render_chain(chain: &WithChain, chain_indent: &str, counter: &mut usize) -> String {
    let mut out = String::new();

    // Detect the body's own indent unit (the leading whitespace beyond the
    // chain indent on the first non-blank body line). Fall back to four
    // spaces only when the body is entirely blank, which is unreachable in
    // valid Python but defensive against the corner case.
    let unit_indent: String = chain
        .body
        .iter()
        .find_map(|line| {
            let trimmed = line.trim_end_matches(['\n', '\r']);
            if trimmed.trim().is_empty() {
                return None;
            }
            let leading_len = trimmed
                .find(|c: char| !c.is_whitespace())
                .unwrap_or(trimmed.len());
            let leading = &trimmed[..leading_len];
            leading
                .strip_prefix(chain_indent)
                .map(|extra| extra.to_owned())
        })
        .unwrap_or_else(|| "    ".to_owned());
    let inner_indent = format!("{}{}", chain_indent, unit_indent);

    for binding in &chain.bindings {
        let tmp = format!("__typhon_with_{}__", *counter);
        *counter += 1;

        out.push_str(chain_indent);
        out.push_str(&tmp);
        out.push_str(" = ");
        out.push_str(&binding.expr);
        out.push('\n');

        out.push_str(chain_indent);
        out.push_str("if isinstance(");
        out.push_str(&tmp);
        out.push_str(", Err):\n");

        match (&chain.err_var, !chain.else_body.is_empty()) {
            (Some(name), true) => {
                // Bind `err = tmp.error` at one indent past the guard,
                // using the detected unit indent.
                out.push_str(&inner_indent);
                out.push_str(name);
                out.push_str(" = ");
                out.push_str(&tmp);
                out.push_str(".error\n");
                // Else-body lines come in with their original indent (one
                // level inside the `else err:` header, which sat at the
                // chain indent). That matches the new `if` body's indent
                // exactly, so emit verbatim.
                for line in &chain.else_body {
                    out.push_str(line);
                }
            }
            _ => {
                out.push_str(&inner_indent);
                out.push_str("return ");
                out.push_str(&tmp);
                out.push('\n');
            }
        }

        out.push_str(chain_indent);
        out.push_str(&binding.target);
        out.push_str(" = ");
        out.push_str(&tmp);
        out.push_str(".value\n");
    }

    // Success body: strip the unit-indent the `with` header imposed on each
    // line so the statements become siblings of the unwraps.
    for line in &chain.body {
        if line.trim().is_empty() {
            out.push_str(line);
        } else if let Some(stripped) = line.strip_prefix(inner_indent.as_str()) {
            out.push_str(chain_indent);
            out.push_str(stripped);
        } else {
            // Indent didn't match — fall back to emitting verbatim so the
            // user sees a precise Python indentation error rather than a
            // silently mangled body.
            out.push_str(line);
        }
    }

    out
}

// ── `|>` pipe operator expansion ──────────────────────────────────────────────

/// Expand the `|>` pipe operator into nested function calls.
///
/// `x |> f` rewrites to `f(x)`. `x |> f(a, b)` rewrites to `f(x, a, b)`.
/// Chained pipes `x |> f |> g` rewrite left-to-right as `g(f(x))`. The
/// transformation runs on the textual source line-by-line before the regular
/// preprocessor.
///
/// Pipes are only rewritten when:
///
/// - They appear outside any string literal or comment.
/// - They appear at parenthesis depth 0 within the line (pipes inside a
///   parenthesised sub-expression must be broken out into their own binding).
/// - The right-hand side is a callable name (`f`, `mod.f`) optionally
///   followed by a parenthesised argument list.
///
/// Lines that begin inside a triple-quoted string are passed through
/// verbatim. A line that contains no top-level `|>` is unchanged.
pub fn expand_pipes(source: &str) -> String {
    let mut result = String::with_capacity(source.len());
    let mut in_string: Option<StringMode> = None;

    for line in source.split_inclusive('\n') {
        let pre_string = in_string;
        let raw = line.trim_end_matches(['\n', '\r']);
        // Preserve the original line terminator (LF, CRLF, or bare-EOF) so
        // mixed-newline files round-trip unchanged.
        let terminator = &line[raw.len()..];
        let code_end = scan_line_code_end(raw, &mut in_string);

        if pre_string.is_some() {
            result.push_str(line);
            continue;
        }

        let code = &raw[..code_end];
        let pipes = find_top_level_pipes(code);
        if pipes.is_empty() {
            result.push_str(line);
            continue;
        }

        let rewritten = match rewrite_pipe_line(code, &pipes) {
            Some(s) => s,
            None => {
                // Bail out — pass the line through unchanged so the regular
                // parser produces a coherent diagnostic at the `|>` token.
                result.push_str(line);
                continue;
            }
        };

        result.push_str(&rewritten);
        // Preserve any trailing comment, then re-attach the original
        // newline bytes (which may be `\r\n` on Windows-authored files).
        result.push_str(&raw[code_end..]);
        result.push_str(terminator);
    }

    result
}

/// Locate every `|>` token in `code` that sits at parenthesis depth 0 and
/// outside any string literal (single-, double-, or triple-quoted).
/// Returns the byte offset of the `|` character in each occurrence.
fn find_top_level_pipes(code: &str) -> Vec<usize> {
    let mut positions = Vec::new();
    let bytes = code.as_bytes();
    scan_inline_code(code, |i, depth, _in_string| {
        if depth == 0
            && bytes[i] == b'|'
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'>'
            // Reject `||>` and `|>=` shapes (neither is valid Python today,
            // but be defensive).
            && (i == 0 || bytes[i - 1] != b'|')
            && (i + 2 >= bytes.len() || bytes[i + 2] != b'=')
        {
            positions.push(i);
            // Tell the scanner to skip the `>` so we don't re-enter this
            // branch on the next byte.
            return Some(2);
        }
        None
    });
    positions
}

/// Walk `code` byte-by-byte, tracking parenthesis depth and string state
/// (including triple-quoted forms). Calls `step(i, depth, in_string)` for
/// every byte position outside a string literal, and inside strings only
/// to allow `step` to inspect the state. `step` may return `Some(skip)` to
/// advance the cursor by `skip` extra bytes (used by token-aware callers
/// like `find_top_level_pipes` once they've matched a multi-byte operator).
///
/// The scanner is byte-level and ASCII-safe: it only inspects bytes that
/// are themselves ASCII (quotes, brackets, the backslash escape, and the
/// matching closing quote), and otherwise transparently advances by one
/// byte. UTF-8 continuation bytes never match any of these so multi-byte
/// characters are passed through correctly.
fn scan_inline_code<F>(code: &str, mut step: F)
where
    F: FnMut(usize, i32, Option<StringMode>) -> Option<usize>,
{
    let bytes = code.as_bytes();
    let mut depth: i32 = 0;
    let mut in_string: Option<StringMode> = None;
    let mut i = 0;
    while i < bytes.len() {
        if let Some(extra) = step(i, depth, in_string) {
            i += extra;
            continue;
        }
        let c = bytes[i];
        match in_string {
            Some(StringMode::Single) | Some(StringMode::Double) => {
                // Single-line strings: honour backslash escapes, look for
                // the matching closing quote.
                let close = match in_string {
                    Some(StringMode::Single) => b'\'',
                    _ => b'"',
                };
                if c == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if c == close {
                    in_string = None;
                }
                i += 1;
            }
            Some(StringMode::TripleSingle) | Some(StringMode::TripleDouble) => {
                let triple: &[u8] = match in_string {
                    Some(StringMode::TripleSingle) => b"'''",
                    _ => b"\"\"\"",
                };
                if i + 3 <= bytes.len() && &bytes[i..i + 3] == triple {
                    in_string = None;
                    i += 3;
                } else {
                    i += 1;
                }
            }
            None => match c {
                b'\'' | b'"' => {
                    let triple: &[u8] = if c == b'"' { b"\"\"\"" } else { b"'''" };
                    if i + 3 <= bytes.len() && &bytes[i..i + 3] == triple {
                        in_string = Some(if c == b'"' {
                            StringMode::TripleDouble
                        } else {
                            StringMode::TripleSingle
                        });
                        i += 3;
                    } else {
                        in_string = Some(if c == b'"' {
                            StringMode::Double
                        } else {
                            StringMode::Single
                        });
                        i += 1;
                    }
                }
                b'(' | b'[' | b'{' => {
                    depth += 1;
                    i += 1;
                }
                b')' | b']' | b'}' => {
                    depth -= 1;
                    i += 1;
                }
                _ => {
                    i += 1;
                }
            },
        }
    }
}

/// Rewrite a single line `code` containing top-level pipe operators (at the
/// byte positions in `pipes`) into the equivalent nested-call form.
///
/// Returns `None` if any pipe right-hand-side does not match the supported
/// callable shape — the caller is expected to pass the line through unchanged.
fn rewrite_pipe_line(code: &str, pipes: &[usize]) -> Option<String> {
    // Split into segments delimited by `|>`. Leading/trailing whitespace on
    // each segment is preserved on the first segment (for indent) but trimmed
    // on intermediate ones.
    let mut segments: Vec<&str> = Vec::with_capacity(pipes.len() + 1);
    let mut last = 0;
    for &pos in pipes {
        segments.push(&code[last..pos]);
        last = pos + 2;
    }
    segments.push(&code[last..]);

    let first = segments[0];
    let indent_end = first
        .find(|c: char| !c.is_whitespace())
        .unwrap_or(first.len());
    let indent = &first[..indent_end];
    let first_body = first[indent_end..].trim_end();

    // Identify any assignment / return prefix on the first segment so the
    // chain only consumes the right-hand expression.
    let (prefix, lhs_expr) = split_pipe_prefix(first_body);
    let lhs_expr = lhs_expr.trim();
    if lhs_expr.is_empty() {
        return None;
    }

    let mut acc = lhs_expr.to_string();
    for seg in &segments[1..] {
        let rhs = seg.trim();
        acc = apply_pipe_call(&acc, rhs)?;
    }

    Some(format!("{}{}{}", indent, prefix, acc))
}

/// Strip an optional `return ` or `LHS [op]= ` prefix from the head of a
/// pipe chain so the chain only consumes the right-hand expression.
///
/// Supports both bare and augmented assignment (`+=`, `-=`, `*=`, `/=`,
/// `%=`, `**=`, `//=`, `<<=`, `>>=`, `&=`, `|=`, `^=`, `@=`). The walrus
/// `:=` is an *expression* operator, not a statement-level assignment, so
/// it is treated as part of the chain LHS rather than a prefix.
fn split_pipe_prefix(s: &str) -> (&str, &str) {
    // `return ` or `return\t`.
    if let Some(rest) = s.strip_prefix("return ") {
        return (&s[..s.len() - rest.len()], rest);
    }
    if let Some(rest) = s.strip_prefix("return\t") {
        return (&s[..s.len() - rest.len()], rest);
    }
    // `yield ` (single-arg pipe yield is unusual but legal).
    if let Some(rest) = s.strip_prefix("yield ") {
        return (&s[..s.len() - rest.len()], rest);
    }
    // Otherwise look for an assignment-statement operator at depth 0.
    if let Some(eq_end) = find_statement_assignment_end(s) {
        let after = &s[eq_end..];
        let trim_start = after.len() - after.trim_start().len();
        return (&s[..eq_end + trim_start], &after[trim_start..]);
    }
    ("", s)
}

/// Locate the byte offset immediately after a statement-level assignment
/// operator at parenthesis depth 0, outside any string literal. Recognises
/// the bare `=` and every augmented form (`+=`, `-=`, `*=`, `/=`, `%=`,
/// `**=`, `//=`, `<<=`, `>>=`, `&=`, `|=`, `^=`, `@=`). Returns `None` for
/// the comparison operators (`==`, `!=`, `<=`, `>=`) and for the walrus
/// `:=` so callers don't mistake them for assignments.
fn find_statement_assignment_end(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_str: Option<char> = None;
    let mut prev_char: Option<char> = None;
    let mut prev_prev_char: Option<char> = None;
    let bytes = s.as_bytes();
    for (i, c) in s.char_indices() {
        if let Some(q) = in_str {
            if c == q {
                in_str = None;
            }
            prev_prev_char = prev_char;
            prev_char = Some(c);
            continue;
        }
        match c {
            '\'' | '"' => in_str = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '=' if depth == 0 => {
                let next = bytes.get(i + 1).copied();
                if next == Some(b'=') {
                    // `==` — comparison, skip.
                } else if prev_char == Some(':') || prev_char == Some('=') {
                    // `:=` (walrus, not a statement-level assignment) or a
                    // trailing `=` inside a longer `==`-style sequence.
                } else if prev_char == Some('!') {
                    // `!=` — comparison.
                } else if prev_char == Some('<') {
                    if prev_prev_char == Some('<') {
                        // `<<=` — augmented assignment.
                        return Some(i + 1);
                    }
                    // `<=` — comparison.
                } else if prev_char == Some('>') {
                    if prev_prev_char == Some('>') {
                        // `>>=` — augmented assignment.
                        return Some(i + 1);
                    }
                    // `>=` — comparison.
                } else {
                    // Bare `=` or augmented (`+=`, `-=`, `*=`, `**=`, `/=`,
                    // `//=`, `%=`, `&=`, `|=`, `^=`, `@=`). All of these are
                    // valid statement-level assignments.
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        prev_prev_char = prev_char;
        prev_char = Some(c);
    }
    None
}

/// Combine an accumulated LHS expression with the next pipe segment.
///
/// - `acc |> name`            → `name(acc)`
/// - `acc |> name(args)`      → `name(acc, args)`  (or `name(acc)` when empty)
/// - `acc |> module.name(..)` → `module.name(acc, ..)`
///
/// Returns `None` for shapes the rewriter does not understand (e.g. lambda or
/// non-call expressions on the RHS).
fn apply_pipe_call(acc: &str, rhs: &str) -> Option<String> {
    let rhs = rhs.trim();
    if rhs.is_empty() {
        return None;
    }

    // Find the first `(` at depth 0 in the RHS (or `None` for bare callable).
    // Uses the shared scanner so triple-quoted strings are handled correctly.
    let bytes = rhs.as_bytes();
    let mut paren_at: Option<usize> = None;
    scan_inline_code(rhs, |i, depth, in_string| {
        if in_string.is_none() && depth == 0 && bytes[i] == b'(' {
            paren_at = Some(i);
            // Signal "stop scanning" by skipping to end.
            return Some(bytes.len() - i);
        }
        None
    });

    match paren_at {
        None => {
            // Bare callable. Validate it looks like an identifier or dotted
            // chain so we don't generate nonsense.
            if !is_dotted_callable(rhs) {
                return None;
            }
            Some(format!("{}({})", rhs, acc))
        }
        Some(open) => {
            let func = rhs[..open].trim_end();
            if !is_dotted_callable(func) {
                return None;
            }
            // Validate the RHS is exactly `func(...)` — i.e. parens span to
            // end of segment. This avoids accidentally rewriting expressions
            // like `f(x) + 1`.
            if !rhs.ends_with(')') {
                return None;
            }
            // Inner args (with no surrounding parens).
            let inner = rhs[open + 1..rhs.len() - 1].trim();
            if inner.is_empty() {
                Some(format!("{}({})", func, acc))
            } else {
                Some(format!("{}({}, {})", func, acc, inner))
            }
        }
    }
}

/// True if `s` looks like a (possibly dotted) callable identifier suitable as
/// the head of a pipe RHS. Used as a guard so we don't rewrite arbitrary
/// expressions that happen to follow `|>` (e.g. `x |> (a + b)`).
fn is_dotted_callable(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut started_segment = false;
    for c in s.chars() {
        if c == '.' {
            if !started_segment {
                return false;
            }
            started_segment = false;
            continue;
        }
        if started_segment {
            if !(c.is_ascii_alphanumeric() || c == '_') {
                return false;
            }
        } else if !(c.is_ascii_alphabetic() || c == '_') {
            return false;
        } else {
            started_segment = true;
        }
    }
    started_segment
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
/// characters inside parentheses/brackets/strings, comparison operators
/// (`==`, `!=`, `<=`, `>=`), augmented assignments (`+=`, `-=`, `*=`, `/=`,
/// `%=`, `&=`, `|=`, `^=`, `**=`, `//=`, `<<=`, `>>=`, `@=`), and the walrus
/// operator (`:=`).
fn find_assignment_eq(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_str: Option<char> = None;
    let chars = s.char_indices();

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
                let prev = if i > 0 { s[..i].chars().last() } else { None };
                let next = s[i + 1..].chars().next();
                // Reject `==`, `!=`, `<=`, `>=` (comparison; `prev == =` also
                // covers `===` chains although Python rejects those), and
                // augmented/walrus operators where `=` is preceded by one of
                // `+ - * / % & | ^ : @`. (`**=`, `//=`, `<<=`, `>>=` are
                // caught because their last char before `=` is one of those
                // single-char operators already.)
                let is_compound = matches!(
                    prev,
                    Some(
                        '!' | '<'
                            | '>'
                            | '='
                            | '+'
                            | '-'
                            | '*'
                            | '/'
                            | '%'
                            | '&'
                            | '|'
                            | '^'
                            | ':'
                            | '@'
                    )
                );
                if !is_compound && !matches!(next, Some('=')) {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

// ── `gather` block expansion ──────────────────────────────────────────────────

/// Strategy selected by the `gather` keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatherStrategy {
    /// Default — lowers to `async with asyncio.TaskGroup()`.
    /// Cancels sibling tasks on the first failure.
    TaskGroup,
    /// `gather(strategy="best-effort"):` — lowers to
    /// `await asyncio.gather(..., return_exceptions=True)`.
    BestEffort,
}

/// Parse the non-indented portion of a `gather` header line into a strategy.
///
/// Recognises `gather:` and `gather(strategy="best-effort"):` / `'best-effort'`.
/// A trailing `# comment` is ignored. Returns `None` for anything else.
fn parse_gather_header(body: &str) -> Option<GatherStrategy> {
    let code = body.split('#').next().unwrap_or("").trim_end();
    if code == "gather:" {
        return Some(GatherStrategy::TaskGroup);
    }
    if code == "gather(strategy=\"best-effort\"):" || code == "gather(strategy='best-effort'):" {
        return Some(GatherStrategy::BestEffort);
    }
    None
}

/// One `NAME = EXPR` assignment parsed from a `gather` body line.
struct GatherBinding {
    name: String,
    expr: String,
}

/// Try to parse a single non-indented gather body line as `NAME = EXPR`.
///
/// Returns `None` for blank lines, comment-only lines, and lines that don't
/// match the expected `identifier = expression` form.
fn parse_gather_binding(content: &str) -> Option<GatherBinding> {
    let content = content.trim();
    if content.is_empty() || content.starts_with('#') {
        return None;
    }

    // Use `find_assignment_eq` which correctly handles string literals and
    // augmented/comparison operators, so `msg = "status=ok"` doesn't split
    // at the `=` inside the string.
    let eq = find_assignment_eq(content)?;
    let name = content[..eq].trim();
    let expr = content[eq + 1..].trim();

    if !is_python_ident(name) || expr.is_empty() {
        return None;
    }

    Some(GatherBinding {
        name: name.to_owned(),
        expr: expr.to_owned(),
    })
}

/// Expand `gather:` and `gather(strategy="best-effort"):` blocks.
///
/// Each `gather:` block contains a sequence of `NAME = EXPR` assignments. The
/// default form lowers to an `asyncio.TaskGroup` block:
///
/// ```text
/// # Typhon
/// gather:
///     user   = fetch_user(id)
///     posts  = fetch_posts(id)
///
/// # Emitted Python
/// async with asyncio.TaskGroup() as __typhon_tg_1:
///     __typhon_t_user_1  = __typhon_tg_1.create_task(fetch_user(id))
///     __typhon_t_posts_1 = __typhon_tg_1.create_task(fetch_posts(id))
/// user  = __typhon_t_user_1.result()
/// posts = __typhon_t_posts_1.result()
/// ```
///
/// The `gather(strategy="best-effort"):` form lowers to
/// `asyncio.gather(..., return_exceptions=True)` and unpacks results by index.
///
/// Lines inside triple-quoted strings are passed through verbatim. A gather
/// block whose body contains non-`NAME = EXPR` lines is left unchanged so the
/// Python parser produces a coherent diagnostic.
pub fn expand_gather(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut counter: usize = 0;
    let mut in_string: Option<StringMode> = None;
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let pre_string = in_string;
        let raw = line.trim_end_matches(['\n', '\r']);
        let _code_end = scan_line_code_end(raw, &mut in_string);

        if pre_string.is_some() {
            out.push_str(line);
            i += 1;
            continue;
        }

        let indent_len = raw.find(|c: char| !c.is_whitespace()).unwrap_or(raw.len());
        let gather_indent = &raw[..indent_len];
        let body = &raw[indent_len..];

        if let Some(strategy) = parse_gather_header(body) {
            // Collect body lines (indented strictly deeper than the gather header).
            let mut bindings: Vec<GatherBinding> = Vec::new();
            let mut all_valid = true;
            let mut j = i + 1;
            let mut body_string = in_string;

            while j < lines.len() {
                let bline = lines[j];
                let braw = bline.trim_end_matches(['\n', '\r']);
                let prev_bstring = body_string;
                let _ = scan_line_code_end(braw, &mut body_string);

                // A line that starts inside a triple-quoted string cannot be a
                // NAME=EXPR binding; bail and leave the whole block verbatim.
                if prev_bstring.is_some() {
                    all_valid = false;
                    break;
                }

                // Empty or comment-only lines are silently skipped.
                if braw.trim().is_empty() || braw.trim().starts_with('#') {
                    j += 1;
                    continue;
                }

                let blen = braw
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(braw.len());

                // Dedented line — the gather body has ended.
                if blen <= indent_len {
                    break;
                }

                let content = &braw[blen..];
                match parse_gather_binding(content) {
                    Some(b) => bindings.push(b),
                    None => {
                        all_valid = false;
                        break;
                    }
                }
                j += 1;
            }

            if all_valid && !bindings.is_empty() {
                counter += 1;
                // All temporaries use a double-underscore prefix with the
                // counter so they cannot collide with user-defined variables.
                let tg = format!("__typhon_tg_{counter}");
                match strategy {
                    GatherStrategy::TaskGroup => {
                        out.push_str(&format!(
                            "{}async with asyncio.TaskGroup() as {}:\n",
                            gather_indent, tg
                        ));
                        for b in &bindings {
                            let task = format!("__typhon_t_{}_{}", b.name, counter);
                            out.push_str(&format!(
                                "{}    {} = {}.create_task({})\n",
                                gather_indent, task, tg, b.expr
                            ));
                        }
                        for b in &bindings {
                            let task = format!("__typhon_t_{}_{}", b.name, counter);
                            out.push_str(&format!(
                                "{}{} = {}.result()\n",
                                gather_indent, b.name, task
                            ));
                        }
                    }
                    GatherStrategy::BestEffort => {
                        let var = format!("__typhon_gather_{counter}");
                        let exprs: Vec<&str> = bindings.iter().map(|b| b.expr.as_str()).collect();
                        out.push_str(&format!(
                            "{}{} = await asyncio.gather({}, return_exceptions=True)\n",
                            gather_indent,
                            var,
                            exprs.join(", ")
                        ));
                        let targets: Vec<&str> = bindings.iter().map(|b| b.name.as_str()).collect();
                        let values: Vec<String> = bindings
                            .iter()
                            .enumerate()
                            .map(|(idx, _)| format!("{}[{}]", var, idx))
                            .collect();
                        out.push_str(&format!(
                            "{}{} = {}\n",
                            gather_indent,
                            targets.join(", "),
                            values.join(", ")
                        ));
                    }
                }
                in_string = body_string;
                i = j;
                continue;
            }
        }

        out.push_str(line);
        i += 1;
    }

    out
}

// ── `go` spawn expansion ──────────────────────────────────────────────────────

/// Expand `go EXPR` and `go EXPR -> NAME` statements into `_typhon_spawn` calls.
///
/// `go f(x)` lowers to `_typhon_spawn(f(x))`.
/// `go f(x) -> fut` lowers to `fut = _typhon_spawn(f(x))`.
///
/// The `go` keyword must be the first token of a statement (after indent).
/// A plain Python `go = value` assignment is never rewritten.
/// Lines inside triple-quoted strings are passed through verbatim.
pub fn expand_go(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut in_string: Option<StringMode> = None;

    for line in source.split_inclusive('\n') {
        let pre_string = in_string;
        let raw = line.trim_end_matches(['\n', '\r']);
        let _code_end = scan_line_code_end(raw, &mut in_string);

        if pre_string.is_some() {
            out.push_str(line);
            continue;
        }

        let indent_len = raw.find(|c: char| !c.is_whitespace()).unwrap_or(raw.len());
        let indent = &raw[..indent_len];
        let body = &raw[indent_len..];
        let terminator = &line[raw.len()..];

        if let Some(rest) = body.strip_prefix("go ") {
            let code = go_strip_comment(rest.trim_end());
            // Reject `go = value` (Python assignment) and other non-expression forms.
            if go_is_expr_start(code) {
                if let Some((expr, target)) = parse_go_arrow(code) {
                    out.push_str(&format!(
                        "{}{} = _typhon_spawn({}){}",
                        indent, target, expr, terminator
                    ));
                    continue;
                }
                out.push_str(&format!("{}_typhon_spawn({}){}", indent, code, terminator));
                continue;
            }
        }

        out.push_str(line);
    }

    out
}

/// Strip a trailing `# comment` from a `go` expression, respecting basic
/// single-quoted strings and backslash escapes. Returns the trimmed code
/// portion.
fn go_strip_comment(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut in_str: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match in_str {
            None => match b {
                b'#' => return s[..i].trim_end(),
                b'"' | b'\'' => in_str = Some(b),
                _ => {}
            },
            Some(q) => {
                if b == b'\\' {
                    i += 1; // skip the escaped character
                } else if b == q {
                    in_str = None;
                }
            }
        }
        i += 1;
    }
    s
}

/// Return `true` if `s` looks like the start of a `go` expression (an
/// identifier, `_`, or `(`), distinguishing it from an assignment (`go = 1`).
fn go_is_expr_start(s: &str) -> bool {
    match s.chars().next() {
        Some(c) => c.is_alphabetic() || c == '_' || c == '(',
        None => false,
    }
}

/// Scan `s` for `-> IDENT` at parenthesis depth 0 and return `(expr, target)`.
///
/// Takes the last such occurrence so that arrow-return annotations on the RHS
/// are handled correctly (though `go f() -> int` is not valid Typhon; users
/// write `go f() -> handle` where `handle` is a variable name, not a type).
///
/// String literals (with backslash-escape awareness) are skipped so that a
/// `->` sequence inside a string argument is not mistaken for the target arrow.
fn parse_go_arrow(s: &str) -> Option<(String, String)> {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut in_str: Option<u8> = None;
    let mut arrow_pos: Option<usize> = None;
    let mut j = 0usize;

    while j < bytes.len() {
        let b = bytes[j];
        match in_str {
            Some(q) => {
                if b == b'\\' {
                    j += 1; // skip escaped character
                } else if b == q {
                    in_str = None;
                }
            }
            None => match b {
                b'"' | b'\'' => in_str = Some(b),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    if depth > 0 {
                        depth -= 1;
                    }
                }
                b'-' if depth == 0 && j + 1 < bytes.len() && bytes[j + 1] == b'>' => {
                    arrow_pos = Some(j);
                    j += 2;
                    continue;
                }
                _ => {}
            },
        }
        j += 1;
    }

    let pos = arrow_pos?;
    let expr = s[..pos].trim_end().to_string();
    let target = s[pos + 2..].trim().to_string();

    if !expr.is_empty() && is_python_ident(&target) {
        return Some((expr, target));
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

    // ── pipe operator expansion ─────────────────────────────────────────────

    #[test]
    fn pipe_bare_callable_wraps_lhs() {
        let out = expand_pipes("y = x |> f\n");
        assert_eq!(out, "y = f(x)\n");
    }

    #[test]
    fn pipe_callable_with_args_prepends_lhs() {
        let out = expand_pipes("y = x |> f(a, b)\n");
        assert_eq!(out, "y = f(x, a, b)\n");
    }

    #[test]
    fn pipe_callable_empty_args_just_passes_lhs() {
        let out = expand_pipes("y = x |> f()\n");
        assert_eq!(out, "y = f(x)\n");
    }

    #[test]
    fn pipe_chains_left_to_right() {
        let out = expand_pipes("z = x |> f |> g\n");
        assert_eq!(out, "z = g(f(x))\n");
    }

    #[test]
    fn pipe_chain_with_mixed_callables() {
        let out = expand_pipes("z = x |> f(1) |> g\n");
        assert_eq!(out, "z = g(f(x, 1))\n");
    }

    #[test]
    fn pipe_return_statement() {
        let out = expand_pipes("return data |> transform |> filter\n");
        assert_eq!(out, "return filter(transform(data))\n");
    }

    #[test]
    fn pipe_expression_statement() {
        let out = expand_pipes("data |> sink\n");
        assert_eq!(out, "sink(data)\n");
    }

    #[test]
    fn pipe_with_dotted_callable() {
        let out = expand_pipes("y = x |> mod.helper(2)\n");
        assert_eq!(out, "y = mod.helper(x, 2)\n");
    }

    #[test]
    fn pipe_preserves_typhon_keyword_prefix() {
        // The `val` keyword survives the rewrite because `expand_pipes` only
        // touches the right-hand side of the assignment.
        let out = expand_pipes("val y = x |> f\n");
        assert_eq!(out, "val y = f(x)\n");
    }

    #[test]
    fn pipe_preserves_type_annotation() {
        let out = expand_pipes("y: int = x |> f\n");
        assert_eq!(out, "y: int = f(x)\n");
    }

    #[test]
    fn pipe_inside_string_is_left_alone() {
        let src = "msg = \"a |> b not a pipe\"\n";
        let out = expand_pipes(src);
        assert_eq!(out, src);
    }

    #[test]
    fn pipe_inside_comment_is_left_alone() {
        let src = "y = f(x)  # consider x |> f\n";
        let out = expand_pipes(src);
        assert_eq!(out, src);
    }

    #[test]
    fn pipe_preserves_indent() {
        let out = expand_pipes("    return x |> f\n");
        assert_eq!(
            out,
            "    return filter_unused\n".replace("filter_unused", "f(x)")
        );
    }

    #[test]
    fn pipe_with_nested_call_in_rhs_args() {
        let out = expand_pipes("y = x |> f(g(1, 2), 3)\n");
        assert_eq!(out, "y = f(x, g(1, 2), 3)\n");
    }

    #[test]
    fn pipe_inside_parens_is_left_alone() {
        // At parenthesis depth > 0, the rewriter declines to act. The Python
        // parser will surface a clear error for the unsupported form.
        let src = "y = sum([a |> f for a in xs])\n";
        let out = expand_pipes(src);
        assert_eq!(out, src);
    }

    #[test]
    fn pipe_non_callable_rhs_passes_through() {
        // `(a + b)` is not a dotted callable; the rewriter leaves the line
        // alone so the user gets a proper syntax error rather than nonsense.
        let src = "y = x |> (a + b)\n";
        let out = expand_pipes(src);
        assert_eq!(out, src);
    }

    #[test]
    fn pipe_no_op_when_no_pipe_present() {
        let src = "y = f(x)\n";
        let out = expand_pipes(src);
        assert_eq!(out, src);
    }

    // ── with-chain expansion ─────────────────────────────────────────────────

    #[test]
    fn with_chain_single_binding_with_else() {
        let src = "\
def run() -> Result[str, str]:
    with x = f()?:
        return Ok(x)
    else err:
        return Err(err)
";
        let out = expand_with_chains(src);
        assert!(out.contains("__typhon_with_0__ = f()"), "out:\n{out}");
        assert!(
            out.contains("if isinstance(__typhon_with_0__, Err):"),
            "out:\n{out}"
        );
        assert!(out.contains("err = __typhon_with_0__.error"), "out:\n{out}");
        assert!(out.contains("return Err(err)"), "out:\n{out}");
        assert!(out.contains("x = __typhon_with_0__.value"), "out:\n{out}");
        assert!(out.contains("return Ok(x)"), "out:\n{out}");
    }

    #[test]
    fn with_chain_multi_binding_threads_temporaries() {
        let src = "\
def run() -> Result[str, str]:
    with a = f()?,
         b = g(a)?:
        return Ok(b)
    else err:
        return Err(err)
";
        let out = expand_with_chains(src);
        assert!(out.contains("__typhon_with_0__ = f()"), "out:\n{out}");
        assert!(out.contains("a = __typhon_with_0__.value"), "out:\n{out}");
        assert!(out.contains("__typhon_with_1__ = g(a)"), "out:\n{out}");
        assert!(out.contains("b = __typhon_with_1__.value"), "out:\n{out}");
    }

    #[test]
    fn with_chain_without_else_falls_back_to_propagation() {
        let src = "\
def run() -> Result[str, str]:
    with x = f()?:
        return Ok(x)
";
        let out = expand_with_chains(src);
        assert!(
            out.contains("return __typhon_with_0__"),
            "no `else` should propagate the raw Err: {out}"
        );
        assert!(
            !out.contains("__typhon_with_0__.error"),
            "unexpected error binding: {out}"
        );
    }

    #[test]
    fn with_chain_bare_else_defaults_err_var() {
        // `else:` without a binding name still allows custom error handling;
        // the desugarer uses the reserved `_err` identifier so the body can
        // reference it.
        let src = "\
def run() -> Result[str, str]:
    with x = f()?:
        return Ok(x)
    else:
        return Err(\"oops\")
";
        let out = expand_with_chains(src);
        assert!(
            out.contains("_err = __typhon_with_0__.error"),
            "out:\n{out}"
        );
        assert!(out.contains("return Err(\"oops\")"), "out:\n{out}");
    }

    #[test]
    fn with_chain_preserves_following_statements() {
        // Anything outside the chain's indentation block must be passed
        // through verbatim so unrelated code is unaffected.
        let src = "\
def run() -> Result[str, str]:
    with x = f()?:
        return Ok(x)
    else err:
        return Err(err)

def other() -> int:
    return 1
";
        let out = expand_with_chains(src);
        assert!(out.contains("def other() -> int:"), "out:\n{out}");
        assert!(out.contains("    return 1"), "out:\n{out}");
    }

    #[test]
    fn pipe_handles_walrus_without_corruption() {
        // The walrus operator `:=` contains `=` and must not be mistaken for
        // an assignment when splitting the chain prefix.
        let src = "y = (z := x) |> f\n";
        let out = expand_pipes(src);
        // No pipe inside parens; the top-level pipe is the only one at depth 0.
        assert_eq!(out, "y = f((z := x))\n");
    }

    #[test]
    fn pipe_inside_triple_quoted_string_left_alone() {
        // A `|>` inside a triple-quoted string must not be treated as a pipe
        // — the apostrophe inside the contents previously confused the
        // single-char string tracker. Ensure the line round-trips unchanged.
        let src = "msg = \"\"\"don't use a |> b here\"\"\"\n";
        let out = expand_pipes(src);
        assert_eq!(out, src);
    }

    #[test]
    fn pipe_after_closing_triple_quote_is_recognised() {
        // A real `|>` *after* a closing `"""` should still be rewritten.
        let src = "y = \"\"\"x\"\"\" |> str.strip\n";
        let out = expand_pipes(src);
        assert_eq!(out, "y = str.strip(\"\"\"x\"\"\")\n");
    }

    #[test]
    fn pipe_preserves_crlf_line_endings() {
        // Windows-authored files use `\r\n`; the rewriter must not silently
        // convert them to `\n`.
        let src = "y = x |> f\r\n";
        let out = expand_pipes(src);
        assert_eq!(out, "y = f(x)\r\n");
    }

    #[test]
    fn find_assignment_eq_rejects_augmented_ops() {
        // `+=`, `*=`, `:=`, `//=`, `**=`, `<<=`, `>>=`, `@=` are not
        // bare assignments — `find_assignment_eq` should return None so the
        // pipe-prefix splitter treats the whole line as a non-assignment.
        assert!(find_assignment_eq("x += 1").is_none());
        assert!(find_assignment_eq("x -= 1").is_none());
        assert!(find_assignment_eq("x *= 2").is_none());
        assert!(find_assignment_eq("x /= 2").is_none());
        assert!(find_assignment_eq("x %= 2").is_none());
        assert!(find_assignment_eq("x **= 2").is_none());
        assert!(find_assignment_eq("x //= 2").is_none());
        assert!(find_assignment_eq("x <<= 2").is_none());
        assert!(find_assignment_eq("x >>= 2").is_none());
        assert!(find_assignment_eq("x &= 2").is_none());
        assert!(find_assignment_eq("x |= 2").is_none());
        assert!(find_assignment_eq("x ^= 2").is_none());
        assert!(find_assignment_eq("x := 2").is_none());
        assert!(find_assignment_eq("x @= m").is_none());
        // But a plain `=` is still found.
        assert_eq!(find_assignment_eq("x = 1"), Some(2));
    }

    #[test]
    fn pipe_with_augmented_assignment_prefix() {
        // `x += a |> f` must split into "prefix = `x += `, chain = `a |> f`"
        // and emit `x += f(a)` — not `x + = ...` (broken Python) and not
        // `f(x += a)` (wraps the assignment).
        assert_eq!(expand_pipes("x += a |> f\n"), "x += f(a)\n");
        assert_eq!(expand_pipes("x *= a |> f\n"), "x *= f(a)\n");
        assert_eq!(expand_pipes("x //= a |> f\n"), "x //= f(a)\n");
        assert_eq!(expand_pipes("x <<= a |> f\n"), "x <<= f(a)\n");
    }

    #[test]
    fn pipe_walrus_in_lhs_not_treated_as_prefix() {
        // `:=` is an expression operator, not a statement assignment, so
        // it must remain part of the chain LHS rather than being split off.
        let out = expand_pipes("y = (z := x) |> f\n");
        assert_eq!(out, "y = f((z := x))\n");
    }

    #[test]
    fn with_chain_indent_detection_uses_two_space_body() {
        // With a two-space body indent, the renderer must strip exactly two
        // spaces (not four) so the success body lines up at the chain indent.
        let src = "\
def run() -> Result[str, str]:
  with x = f()?:
    return Ok(x)
  else err:
    return Err(err)
";
        let out = expand_with_chains(src);
        // The success-body `return Ok(x)` was at 4 spaces (2 outer + 2 inner);
        // after flattening it should sit at 2 spaces.
        assert!(
            out.contains("\n  return Ok(x)"),
            "two-space indent not preserved: {out}"
        );
    }

    // ── lazy import tests ────────────────────────────────────────────────────

    #[test]
    fn preprocess_lazy_import_converts_to_standard_import() {
        let src = "lazy import np = numpy\n";
        let result = preprocess(src);
        assert!(
            result.python_source.contains("import numpy as np"),
            "preprocess should emit `import numpy as np`, got: {}",
            result.python_source
        );
        assert_eq!(result.lazy_imports.len(), 1);
        assert_eq!(result.lazy_imports[0].alias, "np");
        assert_eq!(result.lazy_imports[0].module, "numpy");
    }

    #[test]
    fn preprocess_lazy_import_dotted_module() {
        let src = "lazy import npr = numpy.random\n";
        let result = preprocess(src);
        assert!(
            result.python_source.contains("import numpy.random as npr"),
            "got: {}",
            result.python_source
        );
        assert_eq!(result.lazy_imports[0].module, "numpy.random");
    }

    #[test]
    fn expand_lazy_imports_emits_proxy_class() {
        let src = "lazy import np = numpy\n\nx = np.array([1])\n";
        let out = expand_lazy_imports(src);
        assert!(
            out.contains("class __TyphonLazy_np_"),
            "should emit proxy class, got:\n{out}"
        );
        assert!(out.contains("np = __TyphonLazy_np_()"), "got:\n{out}");
        assert!(
            out.contains("import numpy as _mod"),
            "should import numpy on first use, got:\n{out}"
        );
        // threading is now imported inside __init__, not at module level
        assert!(
            out.contains("import threading as _t"),
            "should include local threading import in __init__, got:\n{out}"
        );
    }

    #[test]
    fn expand_lazy_imports_proxy_has_dir_and_repr() {
        let src = "lazy import np = numpy\n";
        let out = expand_lazy_imports(src);
        assert!(
            out.contains("def __dir__"),
            "proxy should implement __dir__, got:\n{out}"
        );
        assert!(
            out.contains("def __repr__"),
            "proxy should implement __repr__, got:\n{out}"
        );
    }

    #[test]
    fn expand_lazy_imports_no_module_level_threading_for_multiple_lazy_imports() {
        let src = "lazy import np = numpy\nlazy import pd = pandas\n";
        let out = expand_lazy_imports(src);
        // threading must NOT appear at module level — only inside __init__
        assert!(
            !out.starts_with("import threading"),
            "threading must not be at module level, got:\n{out}"
        );
        assert!(
            out.contains("class __TyphonLazy_np_"),
            "np proxy missing, got:\n{out}"
        );
        assert!(
            out.contains("class __TyphonLazy_pd_"),
            "pd proxy missing, got:\n{out}"
        );
    }

    #[test]
    fn expand_lazy_imports_safe_inside_triple_quoted_string() {
        // A `lazy import` that appears literally inside a docstring must NOT
        // be rewritten — it is string content, not a statement.
        let src = "x = \"\"\"\nlazy import np = numpy\n\"\"\"\n";
        let out = expand_lazy_imports(src);
        assert!(
            !out.contains("__TyphonLazy_"),
            "lazy import inside string must not be expanded, got:\n{out}"
        );
        assert!(
            out.contains("lazy import np = numpy"),
            "original text must be preserved"
        );
    }

    #[test]
    fn expand_lazy_imports_trailing_comment_handled() {
        let src = "lazy import np = numpy  # noqa\n";
        let out = expand_lazy_imports(src);
        assert!(
            out.contains("class __TyphonLazy_np_"),
            "trailing comment should not prevent expansion, got:\n{out}"
        );
    }

    #[test]
    fn expand_lazy_imports_passes_through_non_lazy_lines() {
        let src = "import os\nval x: int = 1\n";
        let out = expand_lazy_imports(src);
        assert!(
            out.contains("import os"),
            "non-lazy imports must pass through"
        );
        assert!(
            out.contains("val x: int = 1"),
            "non-lazy lines must pass through"
        );
        assert!(
            !out.contains("threading"),
            "no lazy import should not add threading"
        );
    }

    #[test]
    fn preprocess_lazy_import_not_recognised_with_indentation() {
        // `lazy import` inside a function body is not Typhon syntax and must
        // not be rewritten; the Python parser will handle it as-is.
        let src = "def f():\n    lazy = 1\n";
        let result = preprocess(src);
        assert!(
            result.lazy_imports.is_empty(),
            "indented lazy should not be recorded"
        );
    }

    // ── gather block tests ────────────────────────────────────────────────────

    #[test]
    fn expand_gather_basic_taskgroup() {
        let src = "\
async def fetch_all(id: int):
    gather:
        user = fetch_user(id)
        posts = fetch_posts(id)
    return user, posts
";
        let out = expand_gather(src);
        // Temporaries use counter-scoped __typhon_* names so they cannot
        // collide with user-defined variables.
        assert!(
            out.contains("async with asyncio.TaskGroup() as __typhon_tg_1:"),
            "should emit hygienic TaskGroup context var: {out}"
        );
        assert!(
            out.contains("__typhon_t_user_1 = __typhon_tg_1.create_task(fetch_user(id))"),
            "got: {out}"
        );
        assert!(
            out.contains("__typhon_t_posts_1 = __typhon_tg_1.create_task(fetch_posts(id))"),
            "got: {out}"
        );
        assert!(
            out.contains("user = __typhon_t_user_1.result()"),
            "got: {out}"
        );
        assert!(
            out.contains("posts = __typhon_t_posts_1.result()"),
            "got: {out}"
        );
        assert!(
            !out.contains("gather:"),
            "gather keyword must be removed: {out}"
        );
    }

    #[test]
    fn expand_gather_best_effort() {
        let src = "\
async def fetch_all(id: int):
    gather(strategy=\"best-effort\"):
        user = fetch_user(id)
        posts = fetch_posts(id)
";
        let out = expand_gather(src);
        assert!(
            out.contains("await asyncio.gather("),
            "best-effort should use asyncio.gather: {out}"
        );
        assert!(out.contains("return_exceptions=True"), "got: {out}");
        assert!(out.contains("user, posts ="), "should unpack: {out}");
    }

    #[test]
    fn expand_gather_no_rewrite_outside_block() {
        let src = "val x: int = 1\n";
        let out = expand_gather(src);
        assert_eq!(out, src, "non-gather source must be unchanged");
    }

    #[test]
    fn expand_gather_preserves_non_gather_lines() {
        let src = "import asyncio\nval x: int = 1\n";
        let out = expand_gather(src);
        assert_eq!(out, src, "lines before gather must be unchanged");
    }

    #[test]
    fn expand_gather_inside_string_not_expanded() {
        let src = "x = \"\"\"\ngather:\n    a = f()\n\"\"\"\n";
        let out = expand_gather(src);
        assert!(
            out.contains("gather:"),
            "gather inside string must not be expanded: {out}"
        );
        assert!(
            !out.contains("TaskGroup"),
            "must not emit TaskGroup for string content: {out}"
        );
    }

    // ── go spawn tests ────────────────────────────────────────────────────────

    #[test]
    fn expand_go_simple_call() {
        let src = "go f(x)\n";
        let out = expand_go(src);
        assert_eq!(out, "_typhon_spawn(f(x))\n", "got: {out}");
    }

    #[test]
    fn expand_go_with_arrow_target() {
        let src = "go f(x) -> fut\n";
        let out = expand_go(src);
        assert_eq!(out, "fut = _typhon_spawn(f(x))\n", "got: {out}");
    }

    #[test]
    fn expand_go_preserves_indent() {
        let src = "    go background_task()\n";
        let out = expand_go(src);
        assert_eq!(out, "    _typhon_spawn(background_task())\n", "got: {out}");
    }

    #[test]
    fn expand_go_assignment_not_expanded() {
        let src = "go = 42\n";
        let out = expand_go(src);
        assert_eq!(out, src, "go assignment must not be expanded: {out}");
    }

    #[test]
    fn expand_go_inside_string_not_expanded() {
        let src = "x = \"\"\"\ngo f()\n\"\"\"\n";
        let out = expand_go(src);
        assert!(
            out.contains("go f()"),
            "go inside string must not be expanded: {out}"
        );
        assert!(
            !out.contains("_typhon_spawn"),
            "must not inject spawn for string content: {out}"
        );
    }

    #[test]
    fn with_chain_else_header_without_colon_rejected() {
        // `else err` (missing trailing `:`) is malformed; the chain should be
        // emitted verbatim so the user sees a Python syntax error rather than
        // a silent rewrite that defaults to `_err`.
        let src = "\
def run() -> Result[str, str]:
    with x = f()?:
        return Ok(x)
    else err
        return Err(err)
";
        let out = expand_with_chains(src);
        // No rewrite occurred — the `with` keyword survives in the output.
        assert!(
            out.contains("with x = f()?:"),
            "malformed chain should have been left verbatim: {out}"
        );
        // And no temporary was injected.
        assert!(!out.contains("__typhon_with_"), "should not desugar: {out}");
    }
}
