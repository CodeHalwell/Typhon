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
//! processing.  See `docs/roadmap/phase-5.md` for the longer-term plan.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use tyc_diagnostics::TycError;
use tyc_syntax::{
    ast_equiv::{compare_python_sources, AstComparison, CompareOptions},
    parse_module,
    preprocess::{
        expand_sugar, postprocess_full, preprocess, preprocess_opts, PreprocessOptions,
        StrippedKeyword, StrippedOptional,
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
    //
    // The formatter MUST NOT run any preprocessing pass that shifts
    // line indices, because keyword restoration in `postprocess_full`
    // is indexed by line. The default pipeline expands `impl Alias:`
    // headers targeting a sealed-union alias into one synthesised
    // `impl Variant:` block per variant, duplicating the body and
    // adding (variants - 1) * body_lines new lines. Skip that step
    // here so the post-format file still has the user's original
    // `impl Alias:` header and a single un-duplicated body. (B15.)
    let prep = preprocess_opts(
        source,
        PreprocessOptions {
            expand_impl_sealed_unions: false,
        },
    );

    // Step 2: parse — validate syntax, discard AST.
    //
    // The formatter does not want to rewrite the user's `?`, `|>`, or
    // `with`-chain syntax (that's `tyc build`'s job), but the underlying
    // Python parser cannot accept those constructs directly. Expand them in
    // a throw-away copy purely for validation; the normalised output below
    // is still derived from `prep.python_source` so the Typhon sugar is
    // preserved when the file is rewritten.
    //
    // The throw-away copy is built from the ORIGINAL source through the
    // shared sugar chain and only then preprocessed — the same order every
    // other surface uses. Running the sugar passes over `prep.python_source`
    // instead (preprocess first) inverts that order, and `preprocess`'s
    // nullable-`?` rewrite then eats the postfix `?` operator a later pass
    // needed: `with x = a?,` reaches `expand_with_chains` as
    // `with x = a | None,`, which it cannot recognise, and the formatter
    // rejects a file `tyc check` accepts. Sharing the chain also stops the
    // two from drifting — this copy had fallen two passes behind.
    let validation_input = preprocess(&expand_sugar(source, true)).python_source;
    parse_module(&validation_input).map_err(|e| {
        let offset = usize::from(e.location.start());
        TycError::parse(path, &validation_input, e.to_string(), offset)
    })?;

    // Step 2b: stand a single marker character in for every `?` the
    // preprocessor rewrote to ` | None`.
    //
    // The restore used to be keyed by the byte column of each ` | None` in
    // the pre-processed line. The spacing pass below moves columns
    // (`z:int=r | None` → `z: int = r | None`), so the restore silently
    // missed and the user's `r?` came back as `r | None` — and when the
    // shifted column landed inside a multi-byte character the indexing
    // panicked. A marker character travels with its token through any
    // whitespace edit, so restoration no longer depends on columns at all.
    let marker = optional_marker_for(source);
    let marked = mark_optionals(&prep.python_source, &prep.optionals, marker).ok_or_else(|| {
        TycError::generic(format!(
            "tyc fmt: could not locate a rewritten `?` in '{path}'; the file was left unchanged"
        ))
    })?;

    // Step 3: normalise whitespace on the pre-processed source.
    //
    // Phase 0 normalisation rules:
    //   • Strip trailing whitespace from each line.
    //   • Ensure the file ends with exactly one newline.
    //   • Expand tabs to 4 spaces.
    let (normalised, line_map) = normalise_whitespace_with_map(&marked);

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
    let mut ruff_ran = false;
    let after_ruff = match can_run_ruff.then(ruff_path).flatten() {
        Some(ruff) => match run_ruff_format(&ruff, &normalised, path) {
            Ok(reformatted) => {
                ruff_ran = true;
                reformatted
            }
            Err(msg) => {
                eprintln!("tyc fmt: ruff format failed ({msg}); using in-process output");
                normalised
            }
        },
        None => normalised,
    };

    // Step 4: post-process — restore let/mut keywords, `?` sugar, and lazy imports.
    //
    // `normalise_whitespace` may shift line indices (3+ blank-line
    // collapse and PEP-8 blank-line insertion before top-level defs and
    // classes). The stripped/optional/lazy_import line indices reference
    // the *pre-normalisation* python_source, so translate them through
    // `line_map` before postprocess restores keywords. (B15 / R3.) If
    // ruff ran, line indices are guaranteed identical because the
    // `can_run_ruff` guard above requires the stripped lists to be
    // empty; the translation below is a no-op in that branch.
    let translate =
        |line_index: usize| -> usize { line_map.get(line_index).copied().unwrap_or(line_index) };
    let translated_stripped: Vec<StrippedKeyword> = prep
        .stripped
        .iter()
        .map(|s| StrippedKeyword {
            line_index: translate(s.line_index),
            keyword: s.keyword,
        })
        .collect();
    let translated_lazy: Vec<_> = prep
        .lazy_imports
        .iter()
        .map(|li| tyc_syntax::preprocess::LazyImport {
            line_index: translate(li.line_index),
            alias: li.alias.clone(),
            module: li.module.clone(),
        })
        .collect();
    // `?` sugar is restored from its marker character, not by column (the
    // optionals list is deliberately empty here — see step 2b).
    let output = postprocess_full(&after_ruff, &translated_stripped, &[], &translated_lazy);
    let output = output.replace(marker, "?");

    // Step 4b: repair the builtin-extend restoration.
    //
    // `postprocess_full`'s `Extend` arm only knows how to undo the
    // user-class lowering (`class __typhon_impl_X(object):` → `extend
    // X:`). For an `extend BUILTIN:` header the preprocessor emits a
    // *different* stub — `class __typhon_builtin_ext_BUILTIN(object):`
    // — which that arm does not recognise, so it falls through to its
    // generic branch and produces the broken header
    // `extend class __typhon_builtin_ext_BUILTIN(object):`. Left as-is
    // this leaks an internal lowering into formatted source AND makes
    // `tyc fmt` non-idempotent (the second pass can no longer parse the
    // file). Recover the original `extend BUILTIN:` surface form here so
    // the round-trip is faithful. This correction lives in the formatter
    // crate to keep the change disjoint from the shared preprocess path.
    let output = repair_builtin_extend_restoration(&output);

    // Step 4c: restore `impl …:` / `extend …:` headers verbatim.
    //
    // The preprocess→postprocess round-trip for `impl`/`extend` headers
    // is LOSSY for the generic forms: `impl[T] Dataset[T]:` lowers to
    // `class __typhon_impl_Dataset[T](object):` and restores to
    // `impl Dataset[T]:` (the `impl[T]` prefix is dropped), and a second
    // pass over `impl Dataset[T]:` then drops the target `[T]` too,
    // yielding `impl Dataset:`. That non-invertible lowering makes
    // `tyc fmt` mangle generic impl/extend blocks and breaks idempotence.
    //
    // The formatter has no reason to route these headers through the
    // lossy lowering at all — it only needs to tidy their whitespace.
    // Re-apply the user's ORIGINAL header text (with the same trailing-
    // whitespace trim + tab expansion the normaliser uses) at each
    // recorded header line. The `StrippedKeyword` line indices reference
    // the pre-normalisation source, so translate them through `line_map`
    // exactly as the keyword restoration above does. This keeps the
    // body — which is plain Python and round-trips fine — formatted,
    // while the header survives byte-for-byte (modulo whitespace).
    let original_lines: Vec<&str> = source.lines().collect();
    let output =
        restore_impl_extend_headers_verbatim(&output, &prep.stripped, &original_lines, &translate);

    // Step 5: self-check — refuse to hand back a different program.
    //
    // Every step above is a text edit, and text edits on a language with
    // sugar the parser does not see have corrupted source in four release
    // lines (`?` → `| None`, string-literal contents respaced, …) — in the
    // user's own file, under the one tool people run without reading the
    // diff. Lower the output exactly as the input was lowered, parse it, and
    // compare the two module ASTs. Only `ruff format`'s docstring
    // re-indentation is tolerated, and only when ruff actually ran.
    if output != source {
        verify_same_program(&validation_input, &output, ruff_ran, path)?;
    }

    let changed = output != source;
    Ok(FormatResult { output, changed })
}

/// The formatter's self-check: lower `output` exactly as the input was
/// lowered (`input_lowered` is that lowering of the original source), parse
/// both, and fail unless they denote the same module. `ruff_ran` relaxes the
/// comparison of docstrings to whitespace-insensitive, since `ruff format`
/// re-indents them.
#[allow(clippy::result_large_err)]
fn verify_same_program(
    input_lowered: &str,
    output: &str,
    ruff_ran: bool,
    path: &str,
) -> Result<(), TycError> {
    let output_lowered = preprocess(&expand_sugar(output, true)).python_source;
    let verdict = compare_python_sources(
        input_lowered,
        &output_lowered,
        CompareOptions {
            lenient_docstrings: ruff_ran,
        },
    );
    match verdict {
        AstComparison::Same => Ok(()),
        AstComparison::Different => Err(TycError::generic(format!(
            "tyc fmt: refusing to format '{path}': the formatted text parses to a \
             different program than the original, so the file was left unchanged. \
             This is a formatter bug — please report it at \
             https://github.com/CodeHalwell/Typhon/issues with the file attached."
        ))),
        AstComparison::AfterDoesNotParse(msg) => Err(TycError::generic(format!(
            "tyc fmt: refusing to format '{path}': the formatted text does not parse \
             ({msg}), so the file was left unchanged. This is a formatter bug — please \
             report it at https://github.com/CodeHalwell/Typhon/issues with the file \
             attached."
        ))),
        // The caller parsed the input lowering before formatting anything, so
        // this cannot happen; with nothing to compare against there is also
        // nothing to refuse on.
        AstComparison::BeforeDoesNotParse(_) => Ok(()),
    }
}

/// The text the preprocessor substitutes for a `?` (see
/// `tyc_syntax::preprocess::rewrite_optionals`).
const OPTIONAL_REWRITE: &str = " | None";

/// First private-use code point tried as the `?` stand-in. `U+E000` itself is
/// reserved: [`apply_simple_style_rules_with_paren_depth`] hides string
/// literals behind it.
const FIRST_OPTIONAL_MARKER: u32 = 0xE001;
const LAST_OPTIONAL_MARKER: u32 = 0xF8FF;

/// Pick a private-use character that does not occur anywhere in `source`, so
/// replacing every occurrence of it with `?` after formatting touches only
/// the stand-ins this module inserted.
fn optional_marker_for(source: &str) -> char {
    (FIRST_OPTIONAL_MARKER..=LAST_OPTIONAL_MARKER)
        .filter_map(char::from_u32)
        .find(|c| !source.contains(*c))
        // A file containing all 6,399 private-use characters is not a real
        // input; the first one keeps behaviour defined.
        .unwrap_or('\u{E001}')
}

/// `true` for a `?` stand-in inserted by [`mark_optionals`]. The spacing
/// engine treats it like the identifier character it replaced (`r?` ends an
/// operand exactly as `r` does).
fn is_optional_marker(c: char) -> bool {
    (FIRST_OPTIONAL_MARKER..=LAST_OPTIONAL_MARKER).contains(&(c as u32))
}

/// Replace each ` | None` the preprocessor substituted for a `?` with
/// `marker`. `optionals` carries the exact `(line, byte column)` of every
/// substitution in `python_source`, which is still the buffer they were
/// recorded against, so the columns are valid here (and only here: every
/// later pass may move them). Returns `None` when a recorded position does
/// not hold the rewrite — the caller refuses to format rather than restore
/// the wrong text.
fn mark_optionals(
    python_source: &str,
    optionals: &[StrippedOptional],
    marker: char,
) -> Option<String> {
    if optionals.is_empty() {
        return Some(python_source.to_owned());
    }
    let mut per_line: std::collections::BTreeMap<usize, Vec<usize>> =
        std::collections::BTreeMap::new();
    for opt in optionals {
        per_line
            .entry(opt.line_index)
            .or_default()
            .push(opt.python_col);
    }
    let mut out = String::with_capacity(python_source.len());
    for (idx, line) in python_source.split_inclusive('\n').enumerate() {
        let Some(cols) = per_line.remove(&idx) else {
            out.push_str(line);
            continue;
        };
        let mut line = line.to_owned();
        let mut cols = cols;
        // Right to left, so an earlier column is not moved by a later edit.
        cols.sort_unstable_by(|a, b| b.cmp(a));
        cols.dedup();
        for col in cols {
            if !line
                .get(col..)
                .is_some_and(|tail| tail.starts_with(OPTIONAL_REWRITE))
            {
                return None;
            }
            let mut buf = [0u8; 4];
            line.replace_range(
                col..col + OPTIONAL_REWRITE.len(),
                marker.encode_utf8(&mut buf),
            );
        }
        out.push_str(&line);
    }
    // Every recorded line must exist.
    per_line.is_empty().then_some(out)
}

/// Marker prefix the preprocessor uses for the synthesised stub class of
/// an `extend BUILTIN:` header (e.g. `extend str:` →
/// `class __typhon_builtin_ext_str(object):`).
const BUILTIN_EXTEND_STUB_PREFIX: &str = "__typhon_builtin_ext_";

/// Undo a mis-restored `extend BUILTIN:` header.
///
/// `postprocess_full` does not recognise the builtin-extend stub, so a
/// line that should read `<indent>extend str:` comes back as
/// `<indent>extend class __typhon_builtin_ext_str(object):`. This pass
/// rewrites every such line back to its surface form. The match is
/// deliberately strict — it only fires on the exact
/// `extend class __typhon_builtin_ext_<name>(object)` shape, preserving
/// the original header tail (the trailing `:` and anything after it) so
/// nothing else on the line is disturbed. Lines inside triple-quoted
/// strings cannot match this shape, so no string-awareness is needed.
fn repair_builtin_extend_restoration(source: &str) -> String {
    // Fast path: the stub marker is absent, so there is nothing to fix.
    if !source.contains(BUILTIN_EXTEND_STUB_PREFIX) {
        return source.to_owned();
    }
    let mut out = String::with_capacity(source.len());
    // `split_inclusive` keeps each line's terminator attached, so the
    // exact newline layout (a final blank line, a missing trailing
    // newline, or CRLF terminators) round-trips byte-for-byte. Only the
    // line's code portion is rewritten; the terminator is re-appended
    // verbatim. (A naive `split('\n')` + rejoin double-counts the final
    // newline and grows a trailing blank line on every pass, which would
    // make `tyc fmt` non-idempotent.)
    for line in source.split_inclusive('\n') {
        let (content, term) = match line.strip_suffix('\n') {
            Some(c) => (c.strip_suffix('\r').unwrap_or(c), &line[c.len()..]),
            None => (line, ""),
        };
        out.push_str(&repair_builtin_extend_line(content));
        out.push_str(term);
    }
    out
}

/// Repair a single line if it carries the mis-restored builtin-extend
/// header; otherwise return it unchanged.
fn repair_builtin_extend_line(line: &str) -> String {
    let indent_len = line
        .find(|c: char| !c.is_whitespace())
        .unwrap_or(line.len());
    let indent = &line[..indent_len];
    let rest = &line[indent_len..];
    let Some(after) = rest.strip_prefix("extend class ") else {
        return line.to_owned();
    };
    let Some(after) = after.strip_prefix(BUILTIN_EXTEND_STUB_PREFIX) else {
        return line.to_owned();
    };
    // `after` now looks like `str(object):` (or `list(object):  # cmt`).
    // The synthesised name runs up to the `(object)` marker; everything
    // after it (the `:` and any trailing comment) is the original header
    // tail and must be preserved verbatim.
    let Some(paren_idx) = after.find("(object)") else {
        return line.to_owned();
    };
    let name = &after[..paren_idx];
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return line.to_owned();
    }
    let tail = &after[paren_idx + "(object)".len()..];
    format!("{indent}extend {name}{tail}")
}

/// Overwrite every `impl …:` / `extend …:` header line in `output` with
/// the user's original (whitespace-tidied) header text.
///
/// The preprocess→postprocess lowering for these headers is not
/// invertible for the generic forms (`impl[T] Name[T]:`), so relying on
/// it both mangles the source and breaks idempotence. Instead we keep the
/// body — plain Python that round-trips fine — and splice the original
/// header back in. `stripped` carries the *pre-normalisation* line index
/// of each header; `translate` maps that to the corresponding line in the
/// normalised `output` buffer (the same mapping `postprocess_full` used).
///
/// The spliced header is the original line with trailing whitespace
/// trimmed and leading tabs expanded to four spaces — identical to what
/// the whitespace normaliser does to every other line — so the result is
/// a fixed point: a second `tyc fmt` pass produces byte-identical output.
fn restore_impl_extend_headers_verbatim(
    output: &str,
    stripped: &[StrippedKeyword],
    original_lines: &[&str],
    translate: &impl Fn(usize) -> usize,
) -> String {
    use tyc_syntax::lexer::TyphonKeyword;

    // Collect (output_line_index -> tidied original header) for every
    // impl/extend header. Skip anything that doesn't resolve to a real
    // original line so a stale index can never panic or corrupt output.
    let mut overrides: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
    for s in stripped {
        if !matches!(s.keyword, TyphonKeyword::Impl | TyphonKeyword::Extend) {
            continue;
        }
        let Some(orig) = original_lines.get(s.line_index) else {
            continue;
        };
        let tidied = tidy_header_line(orig);
        // Defensive: only override a line that the lowering actually
        // produced (it must still mention the impl/extend keyword or the
        // lowered class stub). Without a sanity check a wrong index could
        // clobber unrelated code; with it the worst case is a no-op.
        overrides.insert(translate(s.line_index), tidied);
    }
    if overrides.is_empty() {
        return output.to_owned();
    }

    let mut out = String::with_capacity(output.len());
    for (i, line) in output.split_inclusive('\n').enumerate() {
        let (content, term) = match line.strip_suffix('\n') {
            Some(c) => (c.strip_suffix('\r').unwrap_or(c), &line[c.len()..]),
            None => (line, ""),
        };
        match overrides.get(&i) {
            // Only splice when the line at this index still looks like a
            // restored impl/extend header — i.e. it begins (after indent)
            // with `impl`, `extend`, or a leftover lowered stub. This
            // guards against an index drift silently overwriting body
            // code; if the shape doesn't match we keep the formatted line.
            Some(header) if line_is_impl_or_extend_header(content) => {
                out.push_str(header);
            }
            _ => out.push_str(content),
        }
        out.push_str(term);
    }
    out
}

/// Whether `line` (already stripped of its terminator) reads as a
/// restored `impl`/`extend` header at any indentation. Accepts both the
/// surface keywords and a residual lowered `class __typhon_impl_…` /
/// `extend class __typhon_builtin_ext_…` stub in case an earlier pass
/// left one behind.
fn line_is_impl_or_extend_header(line: &str) -> bool {
    let rest = line.trim_start();
    rest.starts_with("impl ")
        || rest.starts_with("impl[")
        || rest.starts_with("extend ")
        || rest.starts_with("class __typhon_impl_")
}

/// Tidy a header line the same way the whitespace normaliser tidies every
/// line: expand leading tabs to four spaces and strip trailing
/// whitespace. The header's interior is left verbatim so the exact
/// `impl[T] Name[T]:` text round-trips.
fn tidy_header_line(line: &str) -> String {
    let trimmed = line.trim_end();
    let indent_end = trimmed
        .find(|c: char| !c.is_whitespace())
        .unwrap_or(trimmed.len());
    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed[..indent_end].chars() {
        if ch == '\t' {
            out.push_str("    ");
        } else {
            out.push(ch);
        }
    }
    out.push_str(&trimmed[indent_end..]);
    out
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
/// Returns a translation map
/// from input line index to the line index of that same logical line in
/// the output. Blank-line collapse (3+ → 2) and PEP-8 blank-line
/// insertion both perturb line indices; the map lets `postprocess_full`
/// keep restoring keywords on the correct lines.
///
/// `map[i]` is the 0-based output line index of input line `i`. Indices
/// past the end of the input are not present.
fn normalise_whitespace_with_map(source: &str) -> (String, Vec<usize>) {
    if source.is_empty() {
        return (String::new(), Vec::new());
    }
    let mut result = String::with_capacity(source.len());
    // Output line counter: increments once per `result.push('\n')`.
    let mut out_line: usize = 0;
    // For each *input* line (i.e. each iteration of the loop below)
    // record which output line index this input line landed on.
    let mut line_map: Vec<usize> = Vec::new();
    let mut consecutive_blank = 0u32;
    // One lexical mask for the whole buffer — the same scanner every
    // preprocessor pass uses — so "does this line start (or end) inside a
    // string literal?" has one answer. This pass used to keep its own
    // triple-quote tracker, which (a) cleared its state on a line like
    // `y""" + """p` without noticing the new literal it opens, and (b) knew
    // nothing of a backslash-continued single-quoted string; both let the
    // spacing rules rewrite the contents of a literal in the user's file.
    let mask = tyc_syntax::lexmask::LexMask::new(source);
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
    // Tracks `(` nesting across lines so multi-line calls keep their
    // kwargs tight (`a=3,` on a continuation line stays `a=3,`, not
    // `a = 3,`). Bracket / brace depth stays line-local because
    // string-content lines get verbatim treatment and `[ ]` slices
    // don't realistically straddle newlines in idiomatic Python.
    let mut paren_depth_carry: i32 = 0;
    for (line_index, raw_line) in source.lines().enumerate() {
        // Is this line *string content* — i.e. does it begin inside a
        // literal that opened on an earlier line (a triple-quoted string, or
        // a single-quoted one continued with a backslash)?
        //
        // If so it gets no normalisation whatsoever. This used to "keep raw,
        // but still strip trailing spaces and expand the leading tabs so
        // indentation matches the file style", which is not formatting at
        // all: it edits the *value* of the literal. A `"""` block containing
        // a tab-indented sample or a line with meaningful trailing spaces came
        // out of `tyc fmt` with different contents — in the user's own source
        // file, in place — and out of `tyc build` with a different constant
        // than the VM had.
        if mask.line_starts_in_string(line_index) {
            // Verbatim, and short-circuit every rule below: blank-line
            // collapsing would eat blank lines out of a docstring, and the
            // tab expansion at the tail would rewrite its indentation.
            line_map.push(out_line);
            result.push_str(raw_line);
            result.push('\n');
            out_line += 1;
            consecutive_blank = 0;
            continue;
        }
        // A line that ends inside a literal (it opens a triple-quoted string,
        // or continues a single-quoted one with a backslash) carries string
        // content up to its end, so its trailing whitespace is data too.
        let ends_in_string = mask.line_starts_in_string(line_index + 1);
        let trimmed = if ends_in_string {
            raw_line
        } else {
            raw_line.trim_end()
        };
        let (line_owned, new_depth) =
            apply_simple_style_rules_with_paren_depth(trimmed, paren_depth_carry);
        paren_depth_carry = new_depth.max(0);

        let line = line_owned.as_str();
        let indent_end = line
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(line.len());
        let leading = &line[..indent_end];
        let rest = &line[indent_end..];

        if rest.is_empty() {
            consecutive_blank += 1;
            if consecutive_blank <= 2 {
                line_map.push(out_line);
                result.push('\n');
                out_line += 1;
            } else {
                // Blank line collapsed; remap to the last emitted output
                // line so stripped entries pointing here don't slide off
                // the end of the buffer.
                line_map.push(out_line.saturating_sub(1));
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
        let is_top_level_decorator = leading.is_empty() && !ends_in_string && rest.starts_with('@');
        let is_top_level_def_or_class = leading.is_empty()
            && !ends_in_string
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
                out_line += 1;
            }
        }
        consecutive_blank = 0;

        line_map.push(out_line);
        for ch in leading.chars() {
            if ch == '\t' {
                result.push_str("    ");
            } else {
                result.push(ch);
            }
        }
        result.push_str(rest);
        result.push('\n');
        out_line += 1;
        emitted_any_code = true;
        prev_top_level_was_decorator = is_top_level_decorator;
    }
    (result, line_map)
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
/// [`normalise_whitespace_with_map`] (file-level pass) so this per-line
/// helper stays local in scope.
///
/// Takes the incoming `paren_depth` carried across lines by the
/// file-level normaliser and returns the residual depth after the line.
/// Continuation lines inside an open `(` get the PEP-8 kwarg rule
/// (`a=1` stays tight) instead of being rewritten to `a = 1`. Bracket /
/// triple-quote state is line-local because triple-quoted spans get
/// verbatim treatment in the outer loop and `[ ]` slices don't
/// realistically straddle a newline in real code.
fn apply_simple_style_rules_with_paren_depth(
    line: &str,
    initial_paren_depth: i32,
) -> (String, i32) {
    // Classify every byte with the preprocessor's shared, PEP 701-aware
    // scanner and hide every string literal — quotes included — and every
    // f-string replacement field behind a placeholder character before the
    // spacing rules run. The rules then see only code (and comments, whose
    // own `#` spacing rule still applies), and the hidden characters are
    // restored one-for-one afterwards.
    //
    // The rule engine's own quote tracking below predates PEP 701 and cannot
    // tell a nested same-quote f-string field (`f"{d["a:b"]}"`) or a format
    // spec (`f"{x:.2f}"`) from code: it rewrote `d["a:b"]` to `d["a: b"]`
    // (a `KeyError` at runtime) and `:.2f` to `: .2f` (a different format —
    // the sign-aware space flag). The masking makes the engine's quote logic
    // unreachable rather than trying to teach it the grammar.
    const PLACEHOLDER: char = '\u{E000}';
    let mut in_string: Option<tyc_syntax::lexmask::StringMode> = None;
    let kinds = tyc_syntax::lexmask::scan_line_kinds(line, &mut in_string);
    let mut masked = String::with_capacity(line.len());
    let mut hidden: Vec<char> = Vec::new();
    for (offset, ch) in line.char_indices() {
        let is_code = kinds.get(offset).is_none_or(|k| {
            matches!(
                k,
                tyc_syntax::lexmask::ByteKind::Code | tyc_syntax::lexmask::ByteKind::Comment
            )
        });
        if is_code {
            masked.push(ch);
        } else {
            masked.push(PLACEHOLDER);
            hidden.push(ch);
        }
    }
    let (styled, depth) = apply_simple_style_rules_unmasked(&masked, initial_paren_depth);
    if hidden.is_empty() {
        return (styled, depth);
    }
    let mut restored = String::with_capacity(styled.len());
    let mut hidden = hidden.into_iter();
    for ch in styled.chars() {
        if ch == PLACEHOLDER {
            // The engine only ever adds or removes whitespace, so every
            // placeholder survives in order.
            restored.push(hidden.next().unwrap_or(PLACEHOLDER));
        } else {
            restored.push(ch);
        }
    }
    (restored, depth)
}

/// The spacing-rule engine proper. Operates on a line whose string,
/// f-string-field and comment bytes have already been replaced by a
/// placeholder character (see [`apply_simple_style_rules_with_paren_depth`]);
/// its own quote tracking is kept for the case of a placeholder-free line but
/// is no longer what keeps the rules out of literals.
fn apply_simple_style_rules_unmasked(line: &str, initial_paren_depth: i32) -> (String, i32) {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let mut quote: Option<char> = None;
    // Triple-quote state.  `Some(q)` means we are inside a `qqq`-bounded
    // string region opened earlier on this same line and the next `qqq` run
    // closes it.  Inside that region everything is passed through verbatim
    // — no `-` spacing, no `=` spacing, nothing.
    let mut triple_quote: Option<char> = None;
    // Track when we've just emitted an opening bracket/paren so the next
    // run of spaces is stripped (`(   x` → `(x`). FINDINGS #65.
    let mut just_opened_bracket = false;
    // Track `[` nesting so `:` inside a slice (`xs[1:2]`) is recognised
    // and NOT given a trailing space. `(` / `{` do not turn slice mode
    // on; only `[` does. Reset on the matching `]`.
    let mut bracket_depth: i32 = 0;
    // Track `(` nesting so `=` inside a call (kwarg) is left tight as
    // PEP 8 §arguments prescribes. The kwarg-vs-default distinction is
    // not tracked — both forms keep `=` tight inside parens. Carries
    // across lines so multi-line calls keep their kwargs tight.
    let mut paren_depth: i32 = initial_paren_depth;
    while let Some(c) = chars.next() {
        // Inside a triple-quoted region: emit verbatim until the matching
        // `qqq` closer.  Triple quotes that open *and* close on the same
        // line as the surrounding code must be passed through whole, or
        // the inner contents get re-formatted as if they were Python
        // tokens.  Without this, `"""…"""` followed by more code on the
        // same line breaks subsequent string literals (PR #120 follow-up:
        // `encoding="utf-8"` was being rewritten to `"utf - 8"`).
        if let Some(q) = triple_quote {
            out.push(c);
            if c == q && chars.peek().copied() == Some(q) {
                // Look one more ahead for the third quote.  Use a clone so
                // we don't consume the iterator until we're sure.
                let mut lookahead = chars.clone();
                lookahead.next();
                if lookahead.peek().copied() == Some(q) {
                    out.push(q);
                    out.push(q);
                    chars.next();
                    chars.next();
                    triple_quote = None;
                }
            }
            continue;
        }
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
                // Detect a triple-quote opener (`"""` / `'''`) before
                // falling back to single-quote handling.  Without this,
                // each `"` in `"""` toggles `quote` independently and the
                // formatter ends up out of sync with the actual string
                // boundaries — see the `triple_quote` branch above for
                // the failure mode this prevents.
                let next1 = chars.peek().copied();
                if next1 == Some(c) {
                    let mut lookahead = chars.clone();
                    lookahead.next();
                    if lookahead.peek().copied() == Some(c) {
                        out.push(c);
                        out.push(c);
                        out.push(c);
                        chars.next();
                        chars.next();
                        // `"""..."""` fully closed on one line is rare in
                        // emitted code but stays correct: the triple_quote
                        // branch above will consume up to the closer.
                        triple_quote = Some(c);
                        just_opened_bracket = false;
                        continue;
                    }
                }
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
                //   `:=` (walrus) — the `:` branch emits the colon without
                //       consuming the `=`, so a bare `:` DOES arrive here as
                //       `prev`. It is listed among the glued operators below.
                //       (The comment used to claim we never see one, and the
                //       `:` was missing from that list, so an unparenthesised
                //       `if n := len(xs):` was rewritten to `if n : = len(xs):`
                //       — `tyc fmt` destroying a working program in place.
                //       A *parenthesised* walrus was safe only by accident,
                //       via the kwarg rule below.)
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
                    Some(':')
                        | Some('+')
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
                //
                // Carve-out: scientific notation. `1e-12` lexes as a
                // single float literal in Python; the `e`/`E` is the
                // exponent marker and the trailing `-` is its sign,
                // not a binary subtraction. `is_binary_operand_lhs`
                // would otherwise see `e` as alphanumeric and insert
                // PEP 8 spaces, producing `1e - 12` — a syntax error.
                let prev = out.chars().last();
                let next = chars.peek().copied();
                if is_scientific_exponent_sign(&out, next) {
                    out.push('-');
                    just_opened_bracket = false;
                    continue;
                }
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
                // Scientific-notation carve-out: `1e+12` is one float
                // literal, see the `-` arm above.
                let prev = out.chars().last();
                let next = chars.peek().copied();
                if is_scientific_exponent_sign(&out, next) {
                    out.push('+');
                    just_opened_bracket = false;
                    continue;
                }
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
    (out, paren_depth)
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
                || is_optional_marker(c)
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

/// True when a `+` / `-` at the current cursor is the sign of a
/// scientific-notation exponent (`1e-12`, `2.5E+7`) rather than a
/// binary or unary arithmetic operator. The previously-emitted run
/// must end with a valid float-literal mantissa followed by `e`/`E`
/// and the next character must be an ASCII digit. We deliberately do
/// NOT count an `e`/`E` preceded by `.` (`.e-1` is not valid Python)
/// but we DO accept the `<digit>.<digit>e` and `.<digit>e` forms by
/// looking past a single `.` when scanning the trailing digit-run.
///
/// A valid Python float-literal mantissa:
/// - must contain at least one digit;
/// - must NOT start with `_` (PEP 515 forbids leading separators);
/// - must NOT be immediately preceded by an identifier character
///   (otherwise the run is the trailing `<digits>e` of an identifier
///   like `value1e` or `_1e`, and the `-` IS a binary operator that
///   still wants PEP 8 spacing).
fn is_scientific_exponent_sign(out: &str, next: Option<char>) -> bool {
    let Some(n) = next else {
        return false;
    };
    if !n.is_ascii_digit() {
        return false;
    }
    let bytes = out.as_bytes();
    let len = bytes.len();
    if len < 2 {
        return false;
    }
    let last = bytes[len - 1];
    if last != b'e' && last != b'E' {
        return false;
    }
    // Walk back from the `e`/`E` over digits / a single `.` / `_`
    // separators. `i` always points at the candidate-mantissa byte we
    // just consumed (or, after a `break`, at the first byte of the
    // mantissa run — the byte AT position `i - 1` is the token
    // boundary if any).
    let mut i = len - 1; // position of `e`/`E`
    let mut saw_digit = false;
    let mut saw_dot = false;
    while i > 0 {
        let c = bytes[i - 1];
        if c.is_ascii_digit() {
            saw_digit = true;
            i -= 1;
            continue;
        }
        if c == b'.' {
            if saw_dot {
                break;
            }
            saw_dot = true;
            i -= 1;
            continue;
        }
        if c == b'_' {
            // `1_000e-3` — PEP 515 numeric separators.
            i -= 1;
            continue;
        }
        break;
    }
    if !saw_digit {
        return false;
    }
    // `i` is now the start of the mantissa run. A leading `_` would
    // make this an identifier (`_1e-3`), not a float literal.
    if bytes[i] == b'_' {
        return false;
    }
    // The byte immediately before the mantissa must NOT be an
    // identifier-continuation character — otherwise the
    // `<digits><e>` run is the trailing slice of an identifier
    // (`abc1e-12`, `value1e-3`) and the `-` is a real binary minus
    // that still wants PEP 8 spacing. Non-ASCII (UTF-8 continuation /
    // start) bytes are conservatively treated as identifier chars
    // since Python identifiers admit Unicode `XID_Continue`.
    if i > 0 {
        let prev = bytes[i - 1];
        if prev.is_ascii_alphanumeric() || prev == b'_' || prev >= 128 {
            return false;
        }
    }
    true
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
        // Pipe operator (`|>`), the `as!` boundary cast, and postfix `?` all
        // survive preprocessing but the stock ruff parser will reject them.
        // A line-internal `|>` / `as!` / `?:` not inside a string is good
        // enough as a heuristic.
        if line.contains("|>") || line.contains("as!") || line.contains("?:") {
            return true;
        }
    }
    false
}

/// Resolve `ruff` to an absolute path on `$PATH`.  Returns `None` when the
/// binary cannot be found, letting the formatter fall back to the in-process
/// pipeline silently.  The `TYC_FMT_DISABLE_RUFF=1` env var forces this to
/// `None` — useful for tests and for deterministic local output.
///
/// Returning the resolved *path* (rather than a bool) is deliberate: the
/// spawn in `run_ruff_format` must use this absolute path, not the bare name
/// `ruff`. On Windows, `Command::new("ruff")` resolves through the
/// CreateProcess search order, which probes the current directory before
/// `$PATH` — so running `tyc fmt` inside an untrusted checkout that ships a
/// `ruff.exe` would execute that binary. Spawning the `$PATH`-resolved path
/// closes that hole (matching `tyc-venv`'s `which_python3`).
fn ruff_path() -> Option<std::path::PathBuf> {
    if std::env::var_os("TYC_FMT_DISABLE_RUFF").is_some_and(|v| v == "1") {
        return None;
    }
    which_on_path("ruff")
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
fn run_ruff_format(ruff: &std::path::Path, source: &str, path: &str) -> Result<String, String> {
    let mut child = Command::new(ruff)
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
///
/// The write is atomic: the formatted output is written to a temporary file in
/// the same directory and then `rename`d over the target. A crash, disk-full,
/// or interruption mid-write therefore leaves the original file intact rather
/// than truncating a user's source (a bare `fs::write` is not crash-safe).
#[allow(clippy::result_large_err)]
pub fn format_file(path: &Path) -> Result<bool, TycError> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| TycError::io(path.to_string_lossy().into_owned(), &e))?;

    let path_str = path.to_string_lossy().into_owned();
    let result = format_source(&source, &path_str)?;

    if result.changed {
        atomic_write(path, result.output.as_bytes())
            .map_err(|e| TycError::io(path.to_string_lossy().into_owned(), &e))?;
    }

    Ok(result.changed)
}

/// Write `bytes` to `path` atomically: write a sibling temp file in the same
/// directory, flush it, then `rename` it over the target. Because the temp file
/// lives on the same filesystem as the target, the rename is atomic, so readers
/// see either the old or the new content — never a half-written file.
///
/// Public so `tyc build` writes its artifacts the same way. A plain
/// `std::fs::write` truncates first, so an interrupted build left a persistent
/// **0-byte** `build/main.py` — which CPython runs successfully, with exit 0
/// and no output. A subsequent build overwrites it, so the failure looks like
/// "the program silently does nothing" rather than "the build was interrupted".
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    // Refuse to write through a symlink. This used to canonicalise the path
    // and rename over the *resolved* file so a user's own link stayed intact
    // — but every caller writes into a tree it did not author (`tyc fmt` on
    // a checkout, `tyc build` into `build/`), and git preserves symlinks, so
    // a pre-planted `build/main.py -> ~/.ssh/authorized_keys` (or a linked
    // `build/` directory) turned a build into an arbitrary file write
    // outside the project, even under `--no-sync` / `TYC_NO_INTROSPECT`.
    // A link at the destination is an error the caller reports; nothing is
    // written and the link is left as it was.
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            return Err(std::io::Error::other(format!(
                "refusing to write through symlink '{}'",
                path.display()
            )));
        }
    }
    let target = path.to_path_buf();

    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let file_name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".to_string());
    // Unique-enough within a run: fmt processes each path at most once, and the
    // pid disambiguates concurrent `tyc fmt` invocations. (No randomness/clock
    // is available in this crate's environment.)
    let tmp = dir.join(format!(".{}.tyc-{}.tmp", file_name, std::process::id()));

    // Scope the handle so it is closed before the rename. `sync_all` (not
    // `flush`, which is a no-op on a bufferless `std::fs::File`) forces the
    // bytes to disk before the rename, so a crash / power loss immediately
    // after can't leave a rename pointing at unwritten data.
    //
    // Create the temp with `create_new` (O_CREAT|O_EXCL): unlike
    // `File::create` (O_CREAT|O_TRUNC) it never follows or truncates an
    // existing path, so a symlink pre-planted at our predictable temp name
    // (e.g. `.main.ty.tyc-<pid>.tmp` → a sensitive file, in an untrusted
    // checkout being formatted) can't redirect the write. A genuinely stale
    // temp from a crashed prior run is unlinked (which removes the entry
    // itself, never a symlink's target) and the create retried exactly once.
    {
        use std::io::ErrorKind;
        let mut f = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                std::fs::remove_file(&tmp)?;
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&tmp)?
            }
            Err(e) => return Err(e),
        };
        f.write_all(bytes)?;
        f.sync_all()?;
    }

    // Preserve the original file's permission bits (e.g. `+x` on a shebang
    // script) — the fresh temp file is created with default perms, so without
    // this the rename would silently strip the executable bit.
    if let Ok(meta) = std::fs::metadata(&target) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }

    match std::fs::rename(&tmp, &target) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Best-effort cleanup of the temp file on failure; keep the
            // original error.
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn unparenthesised_walrus_survives_formatting() {
        // `tyc fmt` rewrote `if n := len(xs):` to `if n : = len(xs):`,
        // destroying a working program in place. The `:` was missing from the
        // glued-operator set the `=` branch checks, and the comment there
        // claimed a bare `:` could never arrive — it does. A *parenthesised*
        // walrus was safe only by accident, through the kwarg rule.
        for src in [
            "if n := len(xs):\n    print(n)\n",
            "while j := step():\n    print(j)\n",
            "y = (m := 3) + 1\n",
        ] {
            let (out, _) = normalise_whitespace_with_map(src);
            assert!(
                out.contains(":="),
                "the walrus must survive formatting; got {out:?}"
            );
            assert!(
                !out.contains(": ="),
                "the walrus must not be split; got {out:?}"
            );
        }
    }

    #[test]
    fn nested_fstring_fields_and_format_specs_are_left_alone() {
        // PEP 701 same-quote nested fields and format specs are not code the
        // spacing rules may touch: `d["a:b"]` must not become `d["a: b"]`
        // (a `KeyError`) and `:.2f` must not become `: .2f` (a different
        // format). The engine's own scanner could not tell; the lexmask can.
        for src in [
            "print(f\"{d[\"content-type\"]} {d[\"a,b\"]} {d[\"a:b\"]} {d[\"x+y\"]}\")\n",
            "print(f\"{f'{x:.2f}'}|\")\n",
            "print(f\"{x:.2f} {y:>8,} {z!r:^10}\")\n",
            // (One space before an inline comment: the engine collapses
            // internal space runs; PEP 8's two is ruff's job.)
            "s = \"a,b:c=d->e\" # x,y:z\n",
            "t = 'it''s' # don't\n",
        ] {
            let (out, _) = normalise_whitespace_with_map(src);
            assert_eq!(out, src, "string / f-string contents must survive verbatim");
        }
        // Ordinary code around a literal is still normalised.
        let (out, _) = normalise_whitespace_with_map("x={\"a:b\":1,\"c\":2}\n");
        assert_eq!(out, "x = {\"a:b\": 1, \"c\": 2}\n");
    }

    #[test]
    fn annotation_and_assignment_spacing_still_applies() {
        // The walrus fix must not disable ordinary `:` / `=` spacing.
        let (out, _) = normalise_whitespace_with_map(
            "def f(a:int,b:str=\"x\")->int:\n    z:int=a\n    return z\n",
        );
        assert!(out.contains("a: int"), "got {out:?}");
        assert!(out.contains("z: int = a"), "got {out:?}");
        assert!(out.contains(") -> int:"), "got {out:?}");
    }

    #[test]
    fn triple_quoted_string_contents_are_never_reformatted() {
        // The whitespace pass used to "keep raw, but still strip trailing
        // spaces and expand the leading tabs" inside a triple-quoted block.
        // That is not formatting — it edits the value of the literal, in the
        // user's own source file, in place under `tyc fmt`.
        let src = "BANNER = \"\"\"\n\tTabbed line\ntrailing spaces here   \n\n\n\nafter three blanks\n\"\"\"\n";
        let (out, _) = normalise_whitespace_with_map(src);
        assert!(
            out.contains("\n\tTabbed line\n"),
            "a tab inside a string must survive; got {out:?}"
        );
        assert!(
            out.contains("trailing spaces here   \n"),
            "trailing spaces inside a string must survive; got {out:?}"
        );
        assert!(
            out.contains("\n\n\n\nafter three blanks"),
            "blank lines inside a string must not be collapsed; got {out:?}"
        );
    }
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
    fn format_typed_tuple_unpack() {
        // kilnlog #4: the formatter's validation parse rejected the typed
        // tuple-unpack form that `tyc check` / `tyc build` accept, because the
        // validation pipeline omitted `expand_typed_let_unpack`. The form must
        // both pass validation and survive verbatim in the output.
        let src = "def use() -> float:\n    let (a: float, b: float) = pair()\n    return a + b\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("let (a: float, b: float) = pair()"),
            "typed tuple-unpack must round-trip through fmt, got:\n{}",
            result.output
        );
    }

    #[test]
    fn format_preserves_checked_cast() {
        // `tyc fmt` validates via the expanded form but emits the surface
        // syntax, so the `as!` cast must survive verbatim (not leak the
        // `__typhon_checked_cast__` lowering).
        let src = "def f(x: object) -> int:\n    let n = x as! int\n    return n\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("as! int"),
            "as! cast must survive fmt, got:\n{}",
            result.output
        );
        assert!(
            !result.output.contains("__typhon_checked_cast__"),
            "the lowering must not leak into formatted source:\n{}",
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
    fn triple_quote_shapes_inside_regular_strings_open_nothing() {
        // A `'''` / `"""` run inside a regular string is text, not an
        // opener: the following lines are still code and get normalised.
        for src in ["x = \"'''\"\ny=1\n", "x = '\"\"\"'\ny=1\n"] {
            let (out, _) = normalise_whitespace_with_map(src);
            assert!(out.ends_with("\ny = 1\n"), "got {out:?}");
        }
        // But a real triple-quote opener still makes the next line string
        // content.
        let (out, _) = normalise_whitespace_with_map("x = \"\"\"hi\ny=1\n\"\"\"\n");
        assert!(out.contains("\ny=1\n"), "got {out:?}");
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
    fn format_preserves_string_after_inline_triple_quote() {
        // PR #120 follow-up: when ruff format reflows `"\n"` to a
        // triple-quoted form on the same line as other code, the third
        // `"` used to flip `apply_simple_style_rules`'s quote tracker into
        // "outside string" state for everything after the triple opener.
        // That made the formatter treat the `-` in a later `"utf-8"` as a
        // binary operator and insert spaces — producing `"utf - 8"`, a
        // codec name Python rejects with LookupError at runtime.
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let src = "f(\"\"\"\n\"\"\".join(xs) + \"\"\"\n\"\"\", encoding=\"utf-8\")\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("\"utf-8\""),
            "string contents after a triple quote must stay verbatim, got: {:?}",
            result.output
        );
        assert!(
            !result.output.contains("\"utf - 8\""),
            "`-` inside string must not gain spaces, got: {:?}",
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
    fn format_keeps_scientific_notation_tight() {
        // R3 carry-over: `1e-12` / `2.5E+7` / `1.0e-12` must stay as a
        // single float literal — the in-process formatter previously
        // saw `e` as alphanumeric and inserted PEP 8 spaces around the
        // sign, producing `1e - 12` (a syntax error). Same fix covers
        // `+` exponents.
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let src = "u = 1e-12\nv = 2.5E+7\nw = 1.0e-12\nz = 1_000e-3\n";
        let result = format_source(src, "<test>").unwrap();
        for needle in ["1e-12", "2.5E+7", "1.0e-12", "1_000e-3"] {
            assert!(
                result.output.contains(needle),
                "expected scientific literal `{}` to round-trip, got: {:?}",
                needle,
                result.output
            );
        }
        for spurious in ["1e -", "1e +", "2.5E +", "1.0e -"] {
            assert!(
                !result.output.contains(spurious),
                "exponent sign must stay tight; saw spurious `{}` in: {:?}",
                spurious,
                result.output
            );
        }
        // Sanity: binary minus on a non-scientific identifier still
        // gains spaces. Three sub-cases that all looked like
        // scientific notation to the original heuristic (each ends in
        // `<digit>e` — but the run is the trailing slice of an
        // identifier, not a float literal): bare `e-1`, an identifier
        // ending in `<digit>e`, and an underscore-prefixed
        // identifier with the same shape.
        let src2 = "let x = e-1\nlet y = abc1e-12\nlet z = _1e-12\n";
        let r2 = format_source(src2, "<test>").unwrap();
        assert!(
            r2.output.contains("e - 1"),
            "binary minus after a bare identifier must space, got: {:?}",
            r2.output
        );
        assert!(
            r2.output.contains("abc1e - 12"),
            "binary minus after `<ident-with-digits>e` must space, got: {:?}",
            r2.output
        );
        assert!(
            r2.output.contains("_1e - 12"),
            "binary minus after `_<digit>e` (identifier, not literal) must space, got: {:?}",
            r2.output
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

    // ── B15 follow-up: `extend BUILTIN:` round-trip + idempotence ─────────

    #[test]
    fn format_preserves_extend_builtin_header() {
        // `extend str:` lowers to a `class __typhon_builtin_ext_str(object):`
        // stub that `postprocess_full` did not know how to restore — the
        // formatter used to emit `extend class __typhon_builtin_ext_str(object):`,
        // leaking the lowering AND breaking the next parse. The surface form
        // must round-trip verbatim.
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let src = "extend str:\n    def slug(self) -> str:\n        return self.lower()\n";
        let result = format_source(src, "<test>").unwrap();
        assert!(
            result.output.contains("extend str:"),
            "extend builtin header must round-trip, got:\n{}",
            result.output
        );
        assert!(
            !result.output.contains("__typhon_builtin_ext_"),
            "the builtin-extend lowering must not leak into formatted source:\n{}",
            result.output
        );
        assert!(
            !result.output.contains("extend class"),
            "the mis-restored `extend class …` header must not survive:\n{}",
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
    fn format_extend_builtin_is_idempotent() {
        // Formatting an `extend BUILTIN:` file twice must reach a fixed
        // point — the second pass is a no-op. This is the regression the
        // whole-corpus idempotence sweep exposed.
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let src = "extend list:\n    def first(self):\n        return self[0]\n";
        let once = format_source(src, "<test>").unwrap().output;
        let twice = format_source(&once, "<test>").unwrap();
        assert_eq!(
            once, twice.output,
            "second format pass must be a no-op; got first:\n{}\nsecond:\n{}",
            once, twice.output
        );
        assert!(
            !twice.changed,
            "already-formatted `extend BUILTIN:` source must report unchanged"
        );
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TYC_FMT_DISABLE_RUFF", v),
                None => std::env::remove_var("TYC_FMT_DISABLE_RUFF"),
            }
        }
    }

    #[test]
    fn repair_builtin_extend_leaves_user_extend_untouched() {
        // A user-class `extend` (no builtin stub) is restored correctly by
        // `postprocess_full`; the repair pass must not touch it. The repair
        // fires only on the exact mis-restored builtin shape.
        assert_eq!(repair_builtin_extend_line("extend User:"), "extend User:");
        assert_eq!(
            repair_builtin_extend_line("    extend Point:"),
            "    extend Point:"
        );
        // The mis-restored builtin shape is repaired, indent + tail kept.
        assert_eq!(
            repair_builtin_extend_line("extend class __typhon_builtin_ext_str(object):"),
            "extend str:"
        );
        assert_eq!(
            repair_builtin_extend_line(
                "    extend class __typhon_builtin_ext_dict(object):  # note"
            ),
            "    extend dict:  # note"
        );
        // A genuine class named with the marker substring inside a string
        // or unrelated context must not be rewritten — the pass keys off
        // the precise `extend class __typhon_builtin_ext_…(object)` head.
        assert_eq!(
            repair_builtin_extend_line("x = \"__typhon_builtin_ext_str\""),
            "x = \"__typhon_builtin_ext_str\""
        );
    }

    /// Representative pre-formatted snippets that must be left byte-for-byte
    /// unchanged by `tyc fmt` (idempotence on already-clean input). Mirrors
    /// the shapes found across `examples/` so the in-process pipeline is a
    /// fixed point on clean code.
    #[test]
    fn format_is_idempotent_on_clean_snippets() {
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let clean = [
            "x: int = 1\n",
            "let y: str = \"hi\"\n",
            "mut count: int = 0\n",
            "z = xs[1:2]\n",
            "w = data[a:b:c]\n",
            "r = f(name=\"Alice\", age=30)\n",
            "def f(x: int, y: int) -> int:\n    return x + y\n",
            "d = {\"a\": 1, \"b\": 2}\n",
            "u = 1e-12\n",
            "extend str:\n    def slug(self) -> str:\n        return self.lower()\n",
        ];
        for src in clean {
            let result = format_source(src, "<test>").unwrap();
            assert_eq!(
                result.output, src,
                "clean snippet must be unchanged by fmt:\n{src:?}\n-> {:?}",
                result.output
            );
            assert!(
                !result.changed,
                "clean snippet must report unchanged:\n{src:?}"
            );
        }
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TYC_FMT_DISABLE_RUFF", v),
                None => std::env::remove_var("TYC_FMT_DISABLE_RUFF"),
            }
        }
    }

    #[test]
    fn format_round_trips_a_freeze_let_with_a_multi_line_string() {
        // `freeze let X = """…"""` lowers to `let X = __typhon_freeze__("""…`
        // with the closing `)` appended to the line that closes the string.
        // The restoration paired that `)` by bracket depth alone, which a
        // triple-quoted RHS never opens — so the opener kept its
        // `__typhon_freeze__(` and `tyc fmt` wrote source it could no
        // longer parse. Round-tripping every shape here proves the pairing
        // now tracks the literal too.
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let sources = [
            // The plain shape: nothing but a string.
            "freeze let BANNER = \"\"\"\nhello\n\"\"\"\n",
            // A `)` inside the string must not be mistaken for the wrapper's.
            "freeze let DOC = \"\"\"\nline with ) paren\n\"\"\"\n",
            // Single-quoted triples take the same path.
            "freeze let TXT = '''a\nb'''\n",
            // A comment on the opener survives the round-trip.
            "freeze let N = \"\"\"\nx\n\"\"\"  # note\n",
            // Brackets AND a literal open on the same line: both have to
            // close before the appended `)` is found.
            "freeze let CFG: dict[str, str] = {\n    \"sql\": \"\"\"\nselect 1)\n\"\"\",\n}\n",
        ];
        for src in sources {
            let once = format_source(src, "<test>").unwrap();
            assert!(
                !once.output.contains("__typhon_freeze__"),
                "the desugaring leaked into formatted source:\n{src:?}\n-> {:?}",
                once.output
            );
            assert_eq!(
                once.output, src,
                "a freeze/string binding must round-trip exactly:\n{src:?}"
            );
            // And the output must still be formattable — the corruption
            // showed up as a parse error on the SECOND pass.
            let twice = format_source(&once.output, "<test>").unwrap();
            assert_eq!(twice.output, once.output, "second pass changed the file");
        }
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TYC_FMT_DISABLE_RUFF", v),
                None => std::env::remove_var("TYC_FMT_DISABLE_RUFF"),
            }
        }
    }

    #[test]
    fn format_accepts_the_sugar_the_checker_accepts() {
        // The validation copy used to run the sugar passes over the
        // ALREADY-preprocessed buffer. `preprocess` rewrites nullable `T?`
        // to `T | None`, so a postfix `?` in a multi-line `with` chain was
        // eaten before `expand_with_chains` could see it and the formatter
        // rejected a file `tyc check` accepts. Building the copy from the
        // original source through the shared chain fixes the order — and
        // sharing it stops the two drifting apart again.
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let sources = [
            // The multi-line `with` chain that regressed.
            concat!(
                "def f(a: Result[str, str], b: Result[int, str]) -> Result[str, str]:\n",
                "    with x = a?,\n",
                "         y = b?:\n",
                "        return Ok(f\"{x}{y}\")\n",
                "    else err:\n",
                "        return Err(err)\n",
            ),
            // A compound `?` header — the pass the copy had fallen behind on.
            concat!(
                "def g(a: Result[int, str]) -> Result[int, str]:\n",
                "    if a? > 0:\n",
                "        return Ok(1)\n",
                "    return Ok(0)\n",
            ),
        ];
        for src in sources {
            let result = format_source(src, "<test>");
            assert!(
                result.is_ok(),
                "the formatter rejected source the checker accepts:\n{src}\n{:?}",
                result.err()
            );
        }
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TYC_FMT_DISABLE_RUFF", v),
                None => std::env::remove_var("TYC_FMT_DISABLE_RUFF"),
            }
        }
    }

    #[test]
    fn format_double_pass_reaches_fixed_point_on_messy_input() {
        // For messy input the FIRST pass may change the file, but a SECOND
        // pass over the already-formatted output must be a no-op. This is
        // the core `tyc fmt --check` idempotence guarantee.
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let messy = [
            "def    f(  x:int,y:int)->int:\n    let    z:int=x+y\n",
            "x = f(name=\"Alice\",age=30)\n",
            "extend dict:\n    def keys2(self):\n        return list(self)\n",
        ];
        for src in messy {
            let once = format_source(src, "<test>").unwrap().output;
            let twice = format_source(&once, "<test>").unwrap();
            assert_eq!(
                once, twice.output,
                "second pass must be a no-op for input:\n{src:?}\nfirst:\n{once}\nsecond:\n{}",
                twice.output
            );
            assert!(
                !twice.changed,
                "second pass must report unchanged for input:\n{src:?}"
            );
        }
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TYC_FMT_DISABLE_RUFF", v),
                None => std::env::remove_var("TYC_FMT_DISABLE_RUFF"),
            }
        }
    }

    /// Run `f` with the external `ruff format` pass disabled, restoring the
    /// previous value of `TYC_FMT_DISABLE_RUFF` afterwards.
    fn without_ruff<T>(f: impl FnOnce() -> T) -> T {
        let _guard = lock_env();
        let prior = std::env::var_os("TYC_FMT_DISABLE_RUFF");
        // SAFETY: serialised by `lock_env`; restored before returning.
        unsafe {
            std::env::set_var("TYC_FMT_DISABLE_RUFF", "1");
        }
        let out = f();
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TYC_FMT_DISABLE_RUFF", v),
                None => std::env::remove_var("TYC_FMT_DISABLE_RUFF"),
            }
        }
        out
    }

    #[test]
    fn triple_quote_closing_and_opening_on_one_line_keeps_the_second_string_verbatim() {
        // W7-02: `line_closes_triple_quote` cleared the in-string state on
        // `y""" + """p` without noticing a new literal opened on the same
        // line, so the second string's contents got comma / `#` spacing and
        // PEP 8 blank lines were inserted before a `def` inside it.
        let src = concat!(
            "a: str = \"\"\"x\n",
            "y\"\"\" + \"\"\"p\n",
            "q,r  s\n",
            "#hash\n",
            "def inner():\n",
            "    pass\"\"\"\n",
            "print(a)\n",
        );
        let (out, _) = normalise_whitespace_with_map(src);
        assert_eq!(out, src, "string contents must survive verbatim");
        let formatted = without_ruff(|| format_source(src, "<test>").unwrap().output);
        assert_eq!(formatted, src);
    }

    #[test]
    fn backslash_continued_string_contents_are_never_reformatted() {
        // W7-02: the shared lexmask reset a single-quoted string at end of
        // line, so the continuation of `"a,b\` + newline was read as code
        // and came back as `c, d e# f"`.
        for src in [
            "b: str = \"a,b\\\n    c,d  e#f\"\nprint(b)\n",
            "b: str = 'x:1\\\n=2,3  #'\nprint(b)\n",
        ] {
            let (out, _) = normalise_whitespace_with_map(src);
            assert_eq!(out, src, "string contents must survive verbatim");
            let formatted = without_ruff(|| format_source(src, "<test>").unwrap().output);
            assert_eq!(formatted, src);
        }
        // Code after the continued string closes is still formatted.
        let (out, _) = normalise_whitespace_with_map("b = \"a\\\nb\"\nc=f(1,2)\n");
        assert_eq!(out, "b = \"a\\\nb\"\nc = f(1, 2)\n");
    }

    #[test]
    fn self_check_refuses_output_that_changes_the_program() {
        // W7-03: the structural guard. Lower the original, then judge a
        // candidate output against it.
        let src =
            "def h(r: Result[int, str]) -> Result[int, str]:\n    let z:int=r?\n    return Ok(z)\n";
        let lowered = preprocess(&expand_sugar(src, true)).python_source;
        // A faithful respacing passes.
        let good = "def h(r: Result[int, str]) -> Result[int, str]:\n    let z: int = r?\n    return Ok(z)\n";
        assert!(verify_same_program(&lowered, good, false, "t.ty").is_ok());
        // The W7-01 corruption (`r?` → `r | None`) is refused.
        let bad = "def h(r: Result[int, str]) -> Result[int, str]:\n    let z: int = r | None\n    return Ok(z)\n";
        let err = verify_same_program(&lowered, bad, false, "t.ty").unwrap_err();
        assert!(err.to_string().contains("refusing to format"), "{err}");
        // A string-content change (the W7-02 corruption) is refused.
        let s_lowered = preprocess(&expand_sugar("b: str = \"c,d  e#f\"\n", true)).python_source;
        assert!(
            verify_same_program(&s_lowered, "b: str = \"c, d e# f\"\n", false, "t.ty").is_err()
        );
        // Output that no longer parses is refused.
        assert!(verify_same_program(&lowered, "def h(:\n", false, "t.ty").is_err());
        // `let` → `mut` is a different program.
        let m_lowered = preprocess(&expand_sugar("let x: int = 1\n", true)).python_source;
        assert!(verify_same_program(&m_lowered, "mut x: int = 1\n", false, "t.ty").is_err());
    }

    #[test]
    fn self_check_tolerates_ruff_docstring_reindent_only_when_ruff_ran() {
        let src = "def f() -> None:\n    \"\"\"Doc.\n\n        more   \n    \"\"\"\n";
        let lowered = preprocess(&expand_sugar(src, true)).python_source;
        let reindented = "def f() -> None:\n    \"\"\"Doc.\n\n    more\n    \"\"\"\n";
        assert!(verify_same_program(&lowered, reindented, true, "t.ty").is_ok());
        assert!(verify_same_program(&lowered, reindented, false, "t.ty").is_err());
    }

    #[test]
    fn format_source_self_check_passes_on_the_sugar_corpus_shapes() {
        // Every Typhon form the formatter round-trips must survive its own
        // guard (a false refusal would make `tyc fmt` unusable).
        let src = concat!(
            "from typing import Protocol\n",
            "lazy import js = json\n",
            "pub let VERSION: str = \"1\"\n",
            "freeze let CFG = {\"a\": [1,2]}\n",
            "newtype UserId = int\n",
            "enum Color: RED; GREEN\n",
            "interface Shape:\n    def area(self) -> float\n",
            "class P frozen:\n    x: float\n",
            "plain class Bag:\n    items: list[str]\n",
            "model Api:\n    id: int\n",
            "impl P:\n    def norm(self) -> float:\n        return self.x\n",
            "def g(x:int) -> Result[int, str]:\n    return Ok(x)\n",
            "def h(a:int,b:str?=None) -> Result[int, str]:\n",
            "    let r:int=g(a)?\n",
            "    let s: str = b if b is not None else \"\"\n",
            "    let t = r |> str()\n",
            "    unsafe:\n        let u = js.loads(\"1\")\n",
            "    return Ok(r + len(s) + len(t))\n",
        );
        let out = without_ruff(|| format_source(src, "<test>").map_err(|e| e.to_string()));
        assert!(
            out.is_ok(),
            "self-check refused a faithful format: {:?}",
            out.err()
        );
    }

    #[test]
    fn format_keeps_postfix_question_when_spacing_shifts_its_column() {
        // W7-01: the preprocessor rewrites every `?` to ` | None` and records
        // the column; the spacing pass then moved that column (`z:int=r` →
        // `z: int = r`) and the column-keyed restore silently skipped, so
        // the user's file came back with `r | None` where `r?` was.
        let src = concat!(
            "def h(x:int) -> Result[int, str]:\n",
            "    let r: Result[int, str] = g(x)\n",
            "    let z:int=r?\n",
            "    print(x,r?)\n",
            "    let o:int?=None\n",
            "    return Ok(z)\n",
        );
        let out = without_ruff(|| format_source(src, "<test>").unwrap().output);
        assert!(out.contains("    let z: int = r?\n"), "got:\n{out}");
        assert!(out.contains("    print(x, r?)\n"), "got:\n{out}");
        assert!(out.contains("    let o: int? = None\n"), "got:\n{out}");
        assert!(!out.contains("| None"), "`?` leaked as `| None`:\n{out}");
    }

    #[test]
    fn format_handles_nullable_of_a_non_ascii_class() {
        // W7-01: the shifted column landed inside `Ü` and the restore indexed
        // the line with it — a "not a char boundary" panic (exit 101).
        let src = "class Üü:\n    v: int\n\ndef f(a:int,b:Üü?) -> int:\n    return a\n";
        let out = without_ruff(|| format_source(src, "<test>").unwrap().output);
        assert!(
            out.contains("def f(a: int, b: Üü?) -> int:\n"),
            "got:\n{out}"
        );
        assert!(!out.contains("| None"), "`?` leaked as `| None`:\n{out}");
    }

    #[test]
    fn format_question_marks_survive_a_marker_char_in_a_string() {
        // The stand-in character must not collide with one the user wrote.
        let src = "let s: str = \"\u{E001}\"\nlet n:int?=None\n";
        let out = without_ruff(|| format_source(src, "<test>").unwrap().output);
        assert_eq!(out, "let s: str = \"\u{E001}\"\nlet n: int? = None\n");
    }
}
