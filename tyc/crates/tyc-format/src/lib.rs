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
//! 4. (Optional) `ruff format` wrapping: when the `ruff` binary is found
//!    on `$PATH`, the normalised pure-Python source is piped through
//!    `ruff format --stdin-filename <path> -`.  Because step 1 has already
//!    stripped every Typhon-only keyword from the buffer ruff sees, ruff
//!    never encounters syntax it can't parse.  If ruff is absent we
//!    silently fall back to the in-process normaliser; if it exits non-zero
//!    we emit a one-line stderr warning and keep the in-process output.
//! 5. Post-process: restore the keywords that *were* stripped (model /
//!    impl / extend / interface / unsafe / comptime / lazy / gather / go).
//!
//! ## Deferred work
//!
//! The Phase-5 roadmap entry calls for a Typhon-aware AST printer wrapped
//! in `ruff format`.  That printer requires a comment-preserving CST and
//! a dedicated emitter — both substantial undertakings — so the present
//! implementation ships the practical halfway point: the existing
//! whitespace normaliser composed with optional `ruff format` post-
//! processing.  See `docs/roadmap.md` §5.7 for the longer-term plan.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use tyc_diagnostics::TycError;
use tyc_syntax::{
    parse_module,
    preprocess::{
        expand_gather_blocks, expand_go_calls, expand_inline_question_ops, expand_multiline_guards,
        expand_pipes, expand_question_ops, expand_with_chains, postprocess_full, preprocess,
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
    let validation_input = expand_question_ops(&expand_inline_question_ops(&expand_pipes(
        &expand_with_chains(&expand_go_calls(&expand_gather_blocks(
            &expand_multiline_guards(&prep.python_source),
        ))),
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

    // Step 3b: (optional) pipe the pure-Python buffer through `ruff format`
    // when the binary is on $PATH AND the buffer contains nothing that the
    // stock ruff Python parser would reject.  Two preconditions:
    //   1. No `prep.stripped` / `prep.optionals` / `prep.lazy_imports` —
    //      these mean `postprocess_full` rewrites lines by index, so any
    //      reformatting that shifts line numbers would corrupt the
    //      restoration.
    //   2. The buffer doesn't contain Typhon-specific tokens that the
    //      preprocessor leaves in place (notably `let `/`mut `: the
    //      vendored ruff parser recognises them, but the stock ruff
    //      binary on `$PATH` does not).
    // The Phase-5 vision will replace this with an AST printer that
    // round-trips Typhon sugar end-to-end.
    let can_run_ruff = prep.stripped.is_empty()
        && prep.optionals.is_empty()
        && prep.lazy_imports.is_empty()
        && !contains_typhon_only_tokens(&normalised);
    let after_ruff = if can_run_ruff && ruff_available() {
        match run_ruff_format(&normalised, path) {
            Ok(reformatted) => reformatted,
            Err(msg) => {
                eprintln!("tyc fmt: ruff format failed ({msg}); using in-process output");
                normalised
            }
        }
    } else {
        normalised
    };

    // Step 4: post-process — restore let/mut keywords, `?` sugar, and lazy imports.
    let output = postprocess_full(
        &after_ruff,
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
    // Track whether any real (non-blank, non-shebang, non-comment) line
    // has been emitted yet so the "two blank lines before top-level
    // def/class" rule doesn't fire at the file head. PEP 8.
    let mut emitted_any_code = false;
    // Track whether the previous emitted code line was a top-level
    // decorator (`@something`). A decorator stack glues to its target
    // `def`/`class`, so we MUST NOT insert two blank lines between
    // `@a` and the next `@b` / `def f` in the stack — that would
    // split the stack into separate orphaned statements and break
    // `tyc fmt` output for any decorated definition. PR #96 P1.
    let mut prev_top_level_was_decorator = false;
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

        // PEP 8 §3 — top-level `def`/`class`/`async def` definitions must
        // be preceded by two blank lines (i.e. three newlines tail). Apply
        // only when:
        //   - we've already emitted at least one code line (no leading
        //     blank gap at the file head), and
        //   - the line is at indent 0 (top-level), not nested inside a
        //     class or function body, and
        //   - the prior line was code (consecutive_blank < 2). When the
        //     user already provided ≥2 blanks the existing branch above
        //     handles it; we only INSERT blanks here when too few exist,
        //     AND
        //   - the prior line was NOT itself a top-level decorator: a
        //     decorator stack belongs to the next `def`/`class`, so
        //     `@cached_property\ndef f(...)` and `@a\n@b\ndef f(...)`
        //     must stay glued (otherwise the formatter splits the
        //     decorator from its target). PR #96 P1.
        let is_top_level_decorator =
            leading.is_empty() && in_triple.is_none() && rest.starts_with('@');
        let is_top_level_def_or_class = leading.is_empty()
            && in_triple.is_none()
            && (rest.starts_with("def ")
                || rest.starts_with("class ")
                || rest.starts_with("async def "));
        let is_top_level_block = is_top_level_decorator || is_top_level_def_or_class;
        if is_top_level_block
            && emitted_any_code
            && consecutive_blank < 2
            && !prev_top_level_was_decorator
        {
            for _ in consecutive_blank..2 {
                result.push('\n');
            }
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
        emitted_any_code = true;
        prev_top_level_was_decorator = is_top_level_decorator;
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
///
/// Beyond the existing whitespace-collapse rules, the pass now adds three
/// PEP 8-style spacing fixes (O12 / FINDINGS #65 / #122 / R3.15 / B9):
///
/// - Insert a space after `,` when followed directly by a non-whitespace
///   token (`(x,y)` → `(x, y)`).
/// - Insert a space after `:` outside slice context (`x:int` → `x: int`,
///   `{"a":1}` → `{"a": 1}`; `xs[1:2]` stays untouched).
/// - Insert spaces around `->` so `()->int:` becomes `() -> int:`.
///
/// PEP 8's two-blank-lines-around-top-level-defs rule lives in
/// [`normalise_whitespace`] (file-level pass) so this per-line helper
/// stays local in scope.
fn apply_simple_style_rules(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let mut quote: Option<char> = None;
    // Track when we've just emitted an opening bracket/paren so the next
    // run of spaces is stripped (`(   x` → `(x`). FINDINGS #65.
    let mut just_opened_bracket = false;
    // Track `[` nesting so `:` inside a slice (`xs[1:2]`) is recognised
    // and NOT given a trailing space. `(` / `{` do not turn slice mode
    // on; only `[` does. Reset on the matching `]`.
    let mut bracket_depth: i32 = 0;
    // Track `(` nesting so `=` inside a call (kwarg) is left tight as
    // PEP 8 §arguments prescribes. The kwarg-vs-default distinction is
    // not tracked — both forms keep `=` tight inside parens.
    let mut paren_depth: i32 = 0;
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
        // Collapse runs of 2+ internal spaces to a single space — but not
        // at the start of the line (indentation must be preserved verbatim).
        // We're past indentation once `out` contains at least one
        // non-whitespace character.
        let past_indent = out.chars().any(|ch| !ch.is_whitespace());
        if c == ' ' && past_indent {
            // Look at the last non-space character to decide whether we
            // should drop this space entirely (after an opening bracket)
            // or keep one (between two tokens).
            let drop_after_open = just_opened_bracket;
            // Consume the rest of the whitespace run.
            while let Some(&n) = chars.peek() {
                if n == ' ' || n == '\t' {
                    chars.next();
                } else {
                    break;
                }
            }
            // Strip the space entirely when:
            //   - we just opened a bracket / paren (collapse `( x` → `(x`)
            //   - the next char is a closing bracket / paren / `,` / `:`
            //     (collapse `x )` → `x)`, `x ,` → `x,`, etc.)
            let next = chars.peek().copied();
            let drop_before_close = matches!(next, Some(')') | Some(']') | Some(',') | Some(';'));
            // Also strip the spaces between every line-leading keyword and
            // the next token (e.g. `def    main` → `def main`) by keeping
            // exactly one space.
            if drop_after_open || drop_before_close {
                // Skip the entire whitespace run; do not emit a space.
            } else {
                out.push(' ');
            }
            just_opened_bracket = false;
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                out.push(c);
                just_opened_bracket = false;
            }
            '(' | '[' => {
                if c == '[' {
                    bracket_depth += 1;
                } else {
                    paren_depth += 1;
                }
                out.push(c);
                just_opened_bracket = true;
                continue;
            }
            ']' => {
                if bracket_depth > 0 {
                    bracket_depth -= 1;
                }
                out.push(']');
                just_opened_bracket = false;
            }
            ')' => {
                if paren_depth > 0 {
                    paren_depth -= 1;
                }
                out.push(')');
                just_opened_bracket = false;
            }
            '=' => {
                // `=` is the most context-sensitive token to space. Cases:
                //   `==` (comparison) — never split, keep tight if user
                //       wrote `==`, add spaces only when user already
                //       spaced one side. Leave alone here.
                //   `:=` (walrus) — handled by the `:` branch swallowing
                //       the `=` chain; we never see a leading `:`.
                //   `+=` / `-=` / `*=` / `/=` / etc. — augmented assign
                //       must stay glued to its operator. Detected by
                //       looking at the previously-emitted char.
                //   `=` inside `(...)` — kwarg / default, PEP 8 keeps it
                //       tight (`f(x=1)`, not `f(x = 1)`).
                //   `=` at the top of a statement — assignment, PEP 8
                //       wants single spaces on each side.
                let next = chars.peek().copied();
                let prev = out.chars().last();
                let is_double_eq = matches!(next, Some('='));
                let is_augmented = matches!(
                    prev,
                    Some('+')
                        | Some('-')
                        | Some('*')
                        | Some('/')
                        | Some('%')
                        | Some('&')
                        | Some('|')
                        | Some('^')
                        | Some('<')
                        | Some('>')
                        | Some('!')
                        | Some('=')
                        | Some('@')
                );
                let is_kwarg = paren_depth > 0;
                if is_double_eq || is_augmented || is_kwarg {
                    out.push('=');
                    just_opened_bracket = false;
                    continue;
                }
                // Plain assignment — single space on each side. The left
                // side first: trim the existing trailing whitespace run
                // back to a single space if any whitespace is present,
                // otherwise insert exactly one space.
                if !matches!(prev, Some(' ') | Some('\t') | None) {
                    out.push(' ');
                }
                out.push('=');
                // Right side: insert a space if not already followed by
                // whitespace; this handles `z:int=x` → `z: int = x`.
                if !matches!(next, Some(' ') | Some('\t') | Some('\n') | None) {
                    out.push(' ');
                }
                just_opened_bracket = false;
            }
            ':' => {
                out.push(':');
                // Inside `[ ]`, `:` is a slice separator — leave it
                // alone. Otherwise (annotation, dict key, block end)
                // insert a space if the next char isn't already one or
                // another `:` (walrus operator handled separately below
                // since `:=` is read by the caller before us — `:` then
                // `=` arrive as two chars and we'd emit `: =` which is
                // wrong, so explicitly skip in that case).
                if bracket_depth == 0 {
                    let next = chars.peek().copied();
                    let needs_space = matches!(next, Some(c) if c != ' ' && c != '\t'
                        && c != ':' && c != '=' && c != '\n' && c != '\r');
                    if needs_space {
                        out.push(' ');
                    }
                }
                just_opened_bracket = false;
            }
            '-' => {
                // `->` return-type arrow. Ensure a single space on each
                // side: `)->int` → `) -> int`, `)  ->int` → `) -> int`.
                // The trailing space is added if the next char isn't
                // already whitespace.
                if let Some(&'>') = chars.peek() {
                    chars.next();
                    if !matches!(out.chars().last(), Some(' ') | Some('\t') | None) {
                        out.push(' ');
                    }
                    out.push_str("->");
                    let next = chars.peek().copied();
                    if !matches!(next, Some(' ') | Some('\t') | Some('\n') | None) {
                        out.push(' ');
                    }
                    just_opened_bracket = false;
                    continue;
                }
                // Binary `-`: insert spaces when prev is an identifier
                // / number / `)` / `]` AND next is an identifier /
                // number / `(` / `[`. Otherwise leave alone (unary
                // `-x`, `[-1]`, `f(-1)` etc.).
                let prev = out.chars().last();
                let next = chars.peek().copied();
                if is_binary_operand_lhs(prev) && is_binary_operand_rhs(next) {
                    if !matches!(prev, Some(' ') | Some('\t')) {
                        out.push(' ');
                    }
                    out.push('-');
                    if !matches!(next, Some(' ') | Some('\t')) {
                        out.push(' ');
                    }
                    just_opened_bracket = false;
                    continue;
                }
                out.push('-');
                just_opened_bracket = false;
            }
            '+' => {
                // Binary `+`: same heuristic as `-`. Unary `+x` (rare
                // but legal Python) and `+=` (compound assignment, the
                // `=` is consumed by its own handler so we never see
                // both characters together here) leave the `+` tight.
                let prev = out.chars().last();
                let next = chars.peek().copied();
                if is_binary_operand_lhs(prev) && is_binary_operand_rhs(next) {
                    if !matches!(prev, Some(' ') | Some('\t')) {
                        out.push(' ');
                    }
                    out.push('+');
                    if !matches!(next, Some(' ') | Some('\t')) {
                        out.push(' ');
                    }
                    just_opened_bracket = false;
                    continue;
                }
                out.push('+');
                just_opened_bracket = false;
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
                just_opened_bracket = false;
            }
            ',' => {
                out.push(',');
                // Collapse runs of whitespace after `,` to a single space.
                // When no whitespace at all follows AND the next char is
                // not a closing bracket (`,)` last-arg stays untouched —
                // PEP 8 allows a trailing comma without trailing space),
                // insert exactly one space (`(x,y)` → `(x, y)`).
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
                let next_non_ws = peek_iter.peek().copied();
                let at_eol =
                    next_non_ws.is_none() || matches!(next_non_ws, Some('\n') | Some('\r'));
                let before_close = matches!(next_non_ws, Some(')') | Some(']') | Some('}'));
                if saw_space {
                    // Consume the whitespace run; emit a single space
                    // unless we're at end-of-line or about to hit a
                    // closing bracket (avoid `, )` artefacts).
                    while let Some(&n) = chars.peek() {
                        if n == ' ' || n == '\t' {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if !at_eol && !before_close {
                        out.push(' ');
                    }
                } else if !at_eol && !before_close {
                    // No whitespace after `,` and the next token is real
                    // code — insert the missing PEP 8 space.
                    out.push(' ');
                }
                just_opened_bracket = false;
            }
            _ => {
                out.push(c);
                // Reset the just-opened-bracket sentinel as soon as we emit
                // any non-bracket, non-space content.  Without this, a
                // genuine inter-token space after `[w` or `(x` (e.g. the
                // space before `for` in `[w for w in xs]`) is mistakenly
                // treated as "still adjacent to the opener" and stripped —
                // turning a valid comprehension into `[wfor w in xs]`.
                just_opened_bracket = false;
            }
        }
    }
    out
}

/// Whether the previously-emitted character looks like the right side of
/// a binary-operator left operand (i.e. an expression-yielding token).
/// Used by the `+` / `-` handlers to decide whether to insert PEP 8
/// spacing or leave the operator tight (unary form).
fn is_binary_operand_lhs(prev: Option<char>) -> bool {
    matches!(
        prev,
        Some(c)
            if c.is_ascii_alphanumeric() || c == '_' || c == ')' || c == ']'
    )
}

/// Whether the next character begins an expression-yielding token —
/// counterpart to [`is_binary_operand_lhs`].
fn is_binary_operand_rhs(next: Option<char>) -> bool {
    matches!(
        next,
        Some(c)
            if c.is_ascii_alphanumeric() || c == '_' || c == '(' || c == '['
    )
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

/// Heuristic check for Typhon-only tokens that the stock `ruff` binary
/// cannot parse.  The vendored ruff parser used by `tyc check` accepts
/// `let`/`mut` natively, but the user's own `ruff` install — which is
/// what runs in `run_ruff_format` — does not.  When this returns true
/// we skip the external `ruff format` pass and keep the in-process
/// output.
///
/// The scan only looks at line-leading tokens (after whitespace), so a
/// `let` appearing inside a string or comment does not trigger a false
/// positive.  It's intentionally a string scan rather than a full
/// tokenisation: precision is unnecessary because the worst case is
/// "skip ruff and use in-process output", which is always safe.
fn contains_typhon_only_tokens(source: &str) -> bool {
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("let ")
            || trimmed.starts_with("mut ")
            || trimmed.starts_with("comptime ")
            || trimmed.starts_with("gather:")
            || trimmed.starts_with("go ")
        {
            return true;
        }
        // Pipe operator (`|>`) and postfix `?` survive preprocessing but the
        // stock ruff parser will reject either.  A line-internal `|>` or a
        // bare `?` that's not inside a string is good enough as a heuristic.
        if line.contains("|>") || line.contains("?:") {
            return true;
        }
    }
    false
}

/// Locate `ruff` on `$PATH`.  Returns `None` when the binary cannot be
/// found, allowing the formatter to fall back to the in-process pipeline
/// silently.  The `TYC_FMT_DISABLE_RUFF=1` env var forces this to `None`
/// — useful for tests and for users who want deterministic local output.
fn ruff_available() -> bool {
    if std::env::var_os("TYC_FMT_DISABLE_RUFF").is_some_and(|v| v == "1") {
        return false;
    }
    which_on_path("ruff").is_some()
}

/// A minimal `which`: scan `$PATH` for an executable named `name`.
/// Falls back to `None` when `$PATH` is unset or the binary is missing.
fn which_on_path(name: &str) -> Option<std::path::PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Pipe `source` through `ruff format --stdin-filename <path> -` and return
/// its stdout.  stderr is captured and discarded (ruff prints a "reformatted"
/// summary there by default).  A non-zero exit yields `Err`.
fn run_ruff_format(source: &str, path: &str) -> Result<String, String> {
    let mut child = Command::new("ruff")
        .arg("format")
        .arg("--stdin-filename")
        .arg(path)
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn: {e}"))?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "stdin not piped".to_owned())?;
        stdin
            .write_all(source.as_bytes())
            .map_err(|e| format!("write stdin: {e}"))?;
    }
    let output = child.wait_with_output().map_err(|e| format!("wait: {e}"))?;
    if !output.status.success() {
        return Err(format!("exit {}", output.status));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("utf-8: {e}"))
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

    /// Serialises every test that mutates `TYC_FMT_DISABLE_RUFF`. Rust
    /// tests run in parallel by default and the env var is process-wide,
    /// so concurrent toggles would race. Holding this mutex for the
    /// duration of the test guarantees one toggle at a time.
    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn format_falls_back_when_ruff_missing() {
        // When ruff is disabled via the env knob, the in-process pipeline
        // must still complete cleanly.  This guards against a regression
        // where the formatter started requiring ruff to be present.
        // SAFETY: tests run in-process; toggling the env briefly is fine
        // because we restore it before exiting the test and hold the env
        // lock for the duration to serialise with peers.
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let src = "let x: int = 1\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(result.output.contains("let x"));
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TYC_FMT_DISABLE_RUFF", v),
                None => std::env::remove_var("TYC_FMT_DISABLE_RUFF"),
            }
        }
    }

    #[test]
    fn format_check_returns_unchanged_for_idempotent_input() {
        // A pre-formatted snippet must round-trip without flipping the
        // `changed` flag — otherwise `tyc fmt --check` would report
        // false-positive diffs on already-clean files.
        let _guard = lock_env();
        let src = "x: int = 1\n";
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let result = format_source(src, "<test>").unwrap();
        assert_eq!(result.output, src);
        assert!(!result.changed);
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TYC_FMT_DISABLE_RUFF", v),
                None => std::env::remove_var("TYC_FMT_DISABLE_RUFF"),
            }
        }
    }

    // ── O12 / FINDINGS #65 / #122 / R3.15 / B9 — three new PEP 8 rules ─────

    #[test]
    fn format_inserts_space_after_colon_in_annotation() {
        // `x:int` → `x: int` outside slice context. The on-PATH ruff
        // could already do this, but the in-process pass guards the
        // result when the user's ruff is missing or disabled.
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let src = "let z:int = 1\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("let z: int = 1"),
            "expected space after `:`, got: {:?}",
            result.output
        );
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TYC_FMT_DISABLE_RUFF", v),
                None => std::env::remove_var("TYC_FMT_DISABLE_RUFF"),
            }
        }
    }

    #[test]
    fn format_leaves_slice_colons_unspaced() {
        // `xs[1:2]` is a slice — `:` inside `[]` must stay tight.
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let src = "y = xs[1:2]\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("xs[1:2]"),
            "slice colons must stay tight, got: {:?}",
            result.output
        );
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TYC_FMT_DISABLE_RUFF", v),
                None => std::env::remove_var("TYC_FMT_DISABLE_RUFF"),
            }
        }
    }

    #[test]
    fn format_inserts_spaces_around_return_arrow() {
        // `)->int:` → `) -> int:` is a top-three eyesore from O12's repro.
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let src = "def f()->int:\n    return 1\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains(") -> int:"),
            "expected spaces around `->`, got: {:?}",
            result.output
        );
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TYC_FMT_DISABLE_RUFF", v),
                None => std::env::remove_var("TYC_FMT_DISABLE_RUFF"),
            }
        }
    }

    #[test]
    fn format_inserts_space_after_missing_comma() {
        // `f(x,y)` → `f(x, y)` even when no whitespace follows the comma.
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let src = "x = f(1,2,3)\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("f(1, 2, 3)"),
            "expected space after each comma, got: {:?}",
            result.output
        );
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TYC_FMT_DISABLE_RUFF", v),
                None => std::env::remove_var("TYC_FMT_DISABLE_RUFF"),
            }
        }
    }

    #[test]
    fn format_inserts_two_blank_lines_before_top_level_def() {
        // PEP 8 §3: two blank lines between top-level definitions. The
        // formatter inserts the missing blanks; an already-correct file
        // is left alone.
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let src = "x: int = 1\ndef f() -> int:\n    return 1\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("\n\n\ndef f()"),
            "expected two blank lines before top-level def, got: {:?}",
            result.output
        );
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TYC_FMT_DISABLE_RUFF", v),
                None => std::env::remove_var("TYC_FMT_DISABLE_RUFF"),
            }
        }
    }

    #[test]
    fn format_leaves_method_bodies_with_single_blank() {
        // Methods nested inside a class are NOT preceded by two blanks
        // — only the top-level def/class is. Verifies the indent-0 gate.
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let src =
            "class Foo:\n    def a(self) -> int:\n        return 1\n    def b(self) -> int:\n        return 2\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            !result.output.contains("\n\n\n    def b"),
            "nested methods must not get two blank lines, got: {:?}",
            result.output
        );
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TYC_FMT_DISABLE_RUFF", v),
                None => std::env::remove_var("TYC_FMT_DISABLE_RUFF"),
            }
        }
    }

    #[test]
    fn format_glues_decorator_stack_to_target() {
        // PR #96 P1: the two-blank-lines rule must NOT split a decorator
        // stack from its target `def`/`class`. `@a\n@b\ndef f(...)` and
        // `@cached_property\ndef f(...)` must stay glued in the
        // emitted Python; otherwise the formatter rewrites valid code
        // into orphaned decorators that no longer apply.
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let src = "x = 1\n@a\n@b\ndef f() -> int:\n    return 1\n";
        let result = format_source(src, "<test>").unwrap();
        // Two blank lines before the first `@a` (top-level boundary).
        assert!(
            result.output.contains("\n\n\n@a\n"),
            "top-level decorator stack should start after two blanks; got: {:?}",
            result.output
        );
        // No blanks between `@a` and `@b`.
        assert!(
            result.output.contains("@a\n@b\n"),
            "stacked decorators must stay glued; got: {:?}",
            result.output
        );
        // No blanks between the last decorator and the `def`.
        assert!(
            result.output.contains("@b\ndef f"),
            "decorator must stay glued to its target def; got: {:?}",
            result.output
        );
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TYC_FMT_DISABLE_RUFF", v),
                None => std::env::remove_var("TYC_FMT_DISABLE_RUFF"),
            }
        }
    }

    #[test]
    fn format_o12_full_repro() {
        // The exact repro in docs/findings.md O12 — every PEP 8 nit on
        // a single line should be corrected by the in-process pass.
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let src = "def    f(  x:int,y:int)->int:\n    let    z:int=x+y\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("def f(x: int, y: int) -> int:"),
            "header should be fully normalised, got: {:?}",
            result.output
        );
        assert!(
            result.output.contains("let z: int = x + y"),
            "body should normalise `z:int=x+y` shape, got: {:?}",
            result.output
        );
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TYC_FMT_DISABLE_RUFF", v),
                None => std::env::remove_var("TYC_FMT_DISABLE_RUFF"),
            }
        }
    }

    #[test]
    fn format_leaves_kwarg_eq_tight() {
        // Inside parens, `=` is a kwarg/default — PEP 8 keeps it tight.
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let src = "x = f(name=\"Alice\", age=30)\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("f(name=\"Alice\", age=30)"),
            "kwarg `=` should stay tight, got: {:?}",
            result.output
        );
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TYC_FMT_DISABLE_RUFF", v),
                None => std::env::remove_var("TYC_FMT_DISABLE_RUFF"),
            }
        }
    }

    #[test]
    fn format_leaves_unary_minus_tight() {
        // `-1` (unary) must not gain spaces; `x - 1` (binary) should.
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let src = "x = -1\ny = x-1\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("x = -1"),
            "unary minus must stay tight, got: {:?}",
            result.output
        );
        assert!(
            result.output.contains("y = x - 1"),
            "binary minus must gain spaces, got: {:?}",
            result.output
        );
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TYC_FMT_DISABLE_RUFF", v),
                None => std::env::remove_var("TYC_FMT_DISABLE_RUFF"),
            }
        }
    }

    #[test]
    fn format_leaves_double_eq_alone() {
        // `==` must not be split into `= =`. Comparison ops are out of
        // scope for this pass.
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let src = "if a==b:\n    pass\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("a==b") || result.output.contains("a == b"),
            "double-eq must not split, got: {:?}",
            result.output
        );
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TYC_FMT_DISABLE_RUFF", v),
                None => std::env::remove_var("TYC_FMT_DISABLE_RUFF"),
            }
        }
    }

    #[test]
    fn format_leaves_augmented_assignment_alone() {
        // `+=` must not get spaces around `=`. The augmented operators
        // are atomic two-char tokens.
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let src = "x += 1\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("x += 1"),
            "augmented assignment must stay tight, got: {:?}",
            result.output
        );
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TYC_FMT_DISABLE_RUFF", v),
                None => std::env::remove_var("TYC_FMT_DISABLE_RUFF"),
            }
        }
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
