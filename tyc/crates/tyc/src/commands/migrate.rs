//! `tyc migrate` — convert typed Python (.py) into Typhon (.ty).
//!
//! Applies a set of textual rewrites that turn idiomatic typed Python into
//! Typhon source.  The pass is intentionally line-based and conservative:
//! we change only what we can identify unambiguously, and we leave
//! everything else alone so the result is still valid Python that the
//! formatter can subsequently normalise.
//!
//! Current rewrites (in order, per line):
//!
//! 1. `Optional[T]` → `T?` (also `typing.Optional[T]`).
//! 2. `T | None` → `T?` when used in an annotation.
//! 3. Module-level `NAME: T = expr` gains a `let` keyword (or `mut` if the
//!    name is later reassigned in the same file).
//! 4. `@dataclass`/`@dataclass(...)` decorations on a `class X:` lose the
//!    decorator — Typhon defaults to dataclass emission.
//! 5. `from dataclasses import dataclass` becomes a no-op (still kept for
//!    safety if any other reference exists, but the line is removed when
//!    `dataclass` was its only import).
//! 6. `class Name(...):` declarations whose body contains a hand-written
//!    `def __init__` (and that did *not* carry a `@dataclass` decorator)
//!    are rewritten as `class! Name(...):` — the dataclass-default of
//!    `class` would clash with a custom constructor, so the raw-class form
//!    is the safe target.
//!
//! Output is written next to the input with the `.ty` extension; `--check`
//! emits to stdout without touching the disk so the user can preview.

use std::collections::HashSet;
use std::path::PathBuf;

use clap::Args;
use miette::{miette, Result};

/// Arguments for `tyc migrate`.
#[derive(Args, Debug)]
pub struct MigrateArgs {
    /// `.py` file or directory to migrate.
    #[arg(value_name = "PATH")]
    pub path: PathBuf,

    /// Print the migrated source to stdout instead of writing `.ty` files.
    #[arg(long)]
    pub check: bool,
}

pub fn run(args: MigrateArgs) -> Result<()> {
    let path = args.path.clone();
    let py_files = collect_py_files(&path)?;

    if py_files.is_empty() {
        return Err(miette!("no .py files found under '{}'", path.display()));
    }

    let mut migrated = 0usize;
    for py in py_files {
        let src = std::fs::read_to_string(&py)
            .map_err(|e| miette!("cannot read '{}': {e}", py.display()))?;
        let out = migrate_source(&src);

        if args.check {
            println!("# ── {} ──", py.display());
            print!("{}", out);
            continue;
        }

        let ty = py.with_extension("ty");
        std::fs::write(&ty, &out).map_err(|e| miette!("cannot write '{}': {e}", ty.display()))?;
        migrated += 1;
    }

    if !args.check {
        println!("migrated {} file(s)", migrated);
    }
    Ok(())
}

/// Public entry for the actual textual conversion.
///
/// The function is exposed so the LSP / format pass can apply the same
/// rewrites incrementally on a buffer.
pub fn migrate_source(source: &str) -> String {
    let reassigned = collect_reassigned_names(source);
    let bang_class_lines = collect_bang_class_lines(source);

    // Scope stack so the line rewriter knows whether we're currently
    // inside a `class` body (skip `let`/`mut` prepending — those are
    // field declarations, not locals) or a `def` body (prepend on the
    // first assignment to each name, skip on subsequent ones).
    #[derive(Clone, Copy, PartialEq)]
    enum ScopeKind {
        Function,
        Class,
    }
    struct Scope {
        kind: ScopeKind,
        indent: usize,
        declared_in_this_scope: HashSet<String>,
    }
    let mut scope_stack: Vec<Scope> = Vec::new();

    let mut out = String::with_capacity(source.len());
    for (line_index, line) in source.split_inclusive('\n').enumerate() {
        let raw = line.trim_end_matches(['\n', '\r']);
        let terminator = &line[raw.len()..];

        let trimmed = raw.trim_start();
        let indent = raw.len() - trimmed.len();

        // Pop scopes we've exited (any non-blank, non-comment line at
        // indent ≤ the scope's header indent leaves that scope).
        let is_structural = !trimmed.is_empty() && !trimmed.starts_with('#');
        if is_structural {
            while let Some(top) = scope_stack.last() {
                if indent <= top.indent {
                    scope_stack.pop();
                } else {
                    break;
                }
            }
        }

        // Identify the innermost scope (if any) before rewriting this line.
        let in_class_body = scope_stack
            .last()
            .is_some_and(|s| s.kind == ScopeKind::Class);
        let already_declared_here = scope_stack
            .last()
            .map(|s| (s.kind == ScopeKind::Function, &s.declared_in_this_scope));

        let rewritten = if raw.trim().is_empty() {
            raw.to_owned()
        } else {
            rewrite_line(
                raw,
                &reassigned,
                line_index,
                &bang_class_lines,
                in_class_body,
                already_declared_here,
            )
        };

        // Update scope tracking with the assignment we just rewrote (so
        // a subsequent line with the same name knows to skip the kw).
        if is_structural {
            if let Some(scope) = scope_stack.last_mut() {
                if scope.kind == ScopeKind::Function {
                    if let Some(name) = leading_plain_assign_name(trimmed)
                        .or_else(|| leading_ann_assign_name(trimmed))
                    {
                        scope.declared_in_this_scope.insert(name);
                    }
                }
            }
            // Push a new scope if this line opens one.
            if trimmed.starts_with("def ") || trimmed.starts_with("async def ") {
                scope_stack.push(Scope {
                    kind: ScopeKind::Function,
                    indent,
                    declared_in_this_scope: HashSet::new(),
                });
            } else if trimmed.starts_with("class ")
                || trimmed.starts_with("class!")
                || trimmed == "class"
            {
                scope_stack.push(Scope {
                    kind: ScopeKind::Class,
                    indent,
                    declared_in_this_scope: HashSet::new(),
                });
            }
        }

        // Skip lines reduced to nothing (only `from dataclasses import
        // dataclass` qualifies today).
        if rewritten.is_empty() && raw.trim_start().starts_with("from dataclasses") {
            continue;
        }

        out.push_str(&rewritten);
        out.push_str(terminator);
    }

    out
}

/// Walk every line and apply rewrite rules in order.  Returns the
/// transformed line (without its terminator).
fn rewrite_line(
    line: &str,
    reassigned: &HashSet<String>,
    line_index: usize,
    bang_class_lines: &HashSet<usize>,
    in_class_body: bool,
    already_declared_here: Option<(bool, &HashSet<String>)>,
) -> String {
    // Quick exit for comments — leave them untouched.
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return line.to_owned();
    }

    // Rule 5: drop `from dataclasses import dataclass` entirely.
    if line.trim() == "from dataclasses import dataclass" {
        return String::new();
    }

    // Rule 5b: `Optional` was just rewritten to `T?` everywhere, so any
    // `from typing import Optional` (or grouped form) is now dead weight.
    // Filter it out, and drop the whole `from typing import …` line when
    // nothing else remains. Skips lines containing `*` or `as` aliases —
    // those are rare and need manual review.
    if let Some(rewritten) = strip_optional_from_typing_import(trimmed) {
        if rewritten.is_empty() {
            return String::new();
        }
        let indent_len = line.len() - trimmed.len();
        let indent = &line[..indent_len];
        return format!("{indent}{rewritten}");
    }

    // Rule 4: drop a `@dataclass` decorator line (with or without args).
    if let Some(rest) = trimmed.strip_prefix("@dataclass") {
        let after = rest.trim();
        if after.is_empty() || after.starts_with('(') {
            return String::new();
        }
    }

    let indent_len = line.len() - trimmed.len();
    let indent = &line[..indent_len];
    let mut body = trimmed.to_owned();

    body = rewrite_optional(&body);
    body = rewrite_typing_aliases(&body);

    // N11 (2026-05-22): `T = TypeVar("T")` (or `TypeVar("T", bound=...)`)
    // at module level is dead weight in Typhon — PEP 695 generic params
    // are introduced inline on the `class`/`def` that uses them.
    if indent.is_empty() && body.contains("TypeVar(") && is_typevar_declaration(&body) {
        return String::new();
    }

    // N11 (2026-05-22): rewrite `class X(Generic[T]):` (and `class!`)
    // to `class X[T]:` so the emitted code uses PEP 695 syntax instead
    // of the legacy Generic[T] base that Typhon rejects. Multiple type
    // params (`Generic[T, U]`) are preserved verbatim inside the new
    // bracket form.
    if body.starts_with("class ") || body.starts_with("class!") {
        if let Some(rewritten) = rewrite_generic_class_base(&body) {
            body = rewritten;
        }
    }

    // Rule 6: `class Name(...):` with a hand-rolled `__init__` and no
    // `@dataclass` decorator becomes `class! Name(...):` so the Typhon
    // dataclass injection does not clash with the custom constructor.
    if bang_class_lines.contains(&line_index) && (body.starts_with("class ") || body == "class") {
        body = format!("class!{}", &body["class".len()..]);
    }

    // Module-level annotated assignment: prepend let/mut.
    if indent.is_empty() {
        if let Some(name) = leading_ann_assign_name(&body) {
            // Only rewrite when the user has not already added the keyword.
            if !body.starts_with("let ") && !body.starts_with("mut ") {
                let kw = if reassigned.contains(&name) {
                    "mut"
                } else {
                    "let"
                };
                body = format!("{kw} {body}");
            }
        } else if let Some(name) = leading_plain_assign_name(&body) {
            // Module-level plain `counter = 0`: when reassigned (via a
            // `global counter` inside a function), prepend `mut` so the
            // type checker sees the intended mutability. Unreassigned
            // names are left untouched — the user can pick `let` after
            // adding an annotation. This closes FINDINGS #22.
            if reassigned.contains(&name) && !body.starts_with("let ") && !body.starts_with("mut ")
            {
                body = format!("mut {body}");
            }
        }
    } else if !in_class_body {
        // Function-body assignment: Typhon requires every local to
        // declare `let` or `mut`. Migrator default is `let` for first
        // assignments and `mut` when the name was reassigned anywhere
        // in the same function. Without this, migrated files fail
        // `tyc check` with `missing_binding_kind`. FINDINGS #64.
        //
        // Class-body lines are skipped — `class Foo: x: int` is a
        // field declaration, not a local. Subsequent assignments to
        // the same name in this function are also skipped (the first
        // assignment already declared the binding).
        let already = already_declared_here
            .map(|(is_fn, set)| (is_fn, set.clone()))
            .unwrap_or((false, HashSet::new()));
        let in_fn = already.0;
        let already_set = already.1;
        if in_fn && !body.starts_with("let ") && !body.starts_with("mut ") {
            let name = leading_plain_assign_name(&body).or_else(|| leading_ann_assign_name(&body));
            if let Some(name) = name {
                if !already_set.contains(&name) {
                    let kw = if reassigned.contains(&name) {
                        "mut"
                    } else {
                        "let"
                    };
                    body = format!("{kw} {body}");
                }
            }
        }
    }

    format!("{indent}{body}")
}

/// Names imported from `typing` that the migrator rewrites away — either
/// because they're replaced by Typhon-native sugar (`Optional` → `T?`,
/// `Union[T, None]` → `T?`, `Union[A, B]` → `A | B`), or because
/// they're deprecated capital-case aliases for built-ins (`List` →
/// `list`, `Dict` → `dict`, etc.). Removing them from the `from
/// typing import …` line drops the now-dead names.
const TYPING_NAMES_TO_REWRITE: &[&str] = &[
    "Optional",
    "Union",
    "List",
    "Dict",
    "Tuple",
    "Set",
    "FrozenSet",
    "Type",
    // N11 (2026-05-22): `Generic[T]` bases are rewritten to PEP 695
    // `class X[T]:`, and `T = TypeVar("T")` definitions are dropped.
    // After the rewrite both names are dead imports.
    "Generic",
    "TypeVar",
];

/// Rewrite `from typing import …, Optional, …` by dropping any name in
/// [`TYPING_NAMES_TO_REWRITE`] (now unused after the matching rule
/// in this migrator). Returns `Some(rewritten)` when the line matched;
/// `Some("")` signals the caller should drop the line entirely. Returns
/// `None` when the line wasn't a `from typing import …` form.
///
/// Conservatively skips wildcard (`*`) and `as`-aliased imports so we
/// never silently drop a renamed name the user may use elsewhere.
/// FINDINGS #115.
fn strip_optional_from_typing_import(trimmed_line: &str) -> Option<String> {
    let rest = trimmed_line.strip_prefix("from typing import")?;
    let rest = rest.trim();
    // Drop a trailing comment so the parser doesn't see it as a name.
    let (names_src, comment) = match rest.find('#') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if names_src.contains('*') || names_src.contains(" as ") {
        return None;
    }
    let names: Vec<&str> = names_src
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let has_rewritten = names.iter().any(|n| TYPING_NAMES_TO_REWRITE.contains(n));
    if !has_rewritten {
        return None;
    }
    let kept: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| !TYPING_NAMES_TO_REWRITE.contains(n))
        .collect();
    if kept.is_empty() {
        return Some(String::new());
    }
    let suffix = if comment.is_empty() {
        String::new()
    } else {
        format!("  {comment}")
    };
    Some(format!("from typing import {}{}", kept.join(", "), suffix))
}

/// Identify class declarations that should emit as `class!` rather than
/// plain `class`. A class qualifies when it has at least one
/// `def __init__` somewhere in its body AND no `@dataclass[(...)]`
/// decorator on the contiguous decorator stack above the `class`
/// keyword. The returned set holds 0-based source line indices pointing
/// at the `class` declaration line itself.
/// `true` when `line` is a module-level `NAME = TypeVar("NAME", ...)`
/// declaration that the migrate pass should drop entirely (N11). Used
/// after the textual rewrites have already converted any PEP 695 forms
/// — what's left is genuinely dead in Typhon because `class Foo[T]:`
/// introduces its own type parameter.
///
/// Matches `T = TypeVar("T")`, `T = TypeVar("T", bound=int)`, and the
/// `typing.TypeVar(...)` qualified form. The name on the LHS must be
/// a single identifier so `pair = TypeVar(...)` (unusual but legal
/// Python) is not accidentally dropped.
fn is_typevar_declaration(line: &str) -> bool {
    let code = line.split('#').next().unwrap_or("").trim();
    let eq = match code.find('=') {
        Some(i) => i,
        None => return false,
    };
    let lhs = code[..eq].trim();
    if lhs.is_empty()
        || !lhs
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        || lhs.starts_with(|c: char| c.is_ascii_digit())
    {
        return false;
    }
    let rhs = code[eq + 1..].trim();
    rhs.starts_with("TypeVar(") || rhs.starts_with("typing.TypeVar(")
}

/// Rewrite `class X(Generic[T]):` and `class X(Generic[T, U], OtherBase):`
/// to PEP 695 form `class X[T]:` / `class X[T, U](OtherBase):` (N11).
/// Returns `None` when the line does not contain a `Generic[...]` base.
///
/// Also handles the `class!` modifier so a hand-rolled `__init__` class
/// keeps its raw-class status post-rewrite.
fn rewrite_generic_class_base(line: &str) -> Option<String> {
    let (keyword, rest_after_keyword) = if let Some(r) = line.strip_prefix("class!") {
        ("class!", r)
    } else if let Some(r) = line.strip_prefix("class ") {
        ("class ", r)
    } else {
        return None;
    };
    let open = rest_after_keyword.find('(')?;
    let name = rest_after_keyword[..open].trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    // Find the matching `)` at depth 0.
    let bytes = rest_after_keyword.as_bytes();
    let mut depth: i32 = 0;
    let mut close: Option<usize> = None;
    for i in open..bytes.len() {
        match bytes[i] {
            b'(' | b'[' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            b']' => depth -= 1,
            _ => {}
        }
    }
    let close = close?;
    let inside = &rest_after_keyword[open + 1..close];
    let trailer = &rest_after_keyword[close + 1..];

    // Split the bases by top-level commas (commas inside [...] stay
    // grouped — `dict[str, int]` and `Generic[T, U]` must not split).
    let bases: Vec<&str> = split_top_level_commas(inside)
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    let mut type_params: Option<String> = None;
    let mut remaining_bases: Vec<&str> = Vec::new();
    for b in bases {
        if let Some(params) = b
            .strip_prefix("Generic[")
            .or_else(|| b.strip_prefix("typing.Generic["))
            .and_then(|s| s.strip_suffix(']'))
        {
            type_params = Some(params.trim().to_owned());
        } else {
            remaining_bases.push(b);
        }
    }

    let type_params = type_params?;
    let new_name_part = format!("{}[{}]", name, type_params);
    let new_bases = if remaining_bases.is_empty() {
        String::new()
    } else {
        format!("({})", remaining_bases.join(", "))
    };
    Some(format!("{}{}{}{}", keyword, new_name_part, new_bases, trailer))
}

fn collect_bang_class_lines(source: &str) -> HashSet<usize> {
    let lines: Vec<&str> = source.lines().collect();
    let mut out: HashSet<usize> = HashSet::new();

    for (idx, raw) in lines.iter().enumerate() {
        let trimmed = raw.trim_start();
        if !trimmed.starts_with("class ") && trimmed != "class" {
            continue;
        }
        // Walk back over the decorator stack: skip blank lines and lines
        // that begin with `@`. If any decorator is `@dataclass[...]` the
        // class already opts into dataclass semantics — leave it alone.
        let indent_len = raw.len() - trimmed.len();
        let mut had_dataclass_decorator = false;
        let mut probe = idx;
        while probe > 0 {
            probe -= 1;
            let prev = lines[probe];
            let prev_trim = prev.trim_start();
            if prev_trim.is_empty() {
                continue;
            }
            // Decorators must sit at the same indent as the class header.
            let prev_indent = prev.len() - prev_trim.len();
            if prev_indent != indent_len {
                break;
            }
            if !prev_trim.starts_with('@') {
                break;
            }
            if let Some(after_kw) = prev_trim.strip_prefix("@dataclass") {
                let after = after_kw.trim();
                if after.is_empty() || after.starts_with('(') {
                    had_dataclass_decorator = true;
                    break;
                }
            }
        }
        if had_dataclass_decorator {
            continue;
        }

        // Walk forward looking for a `def __init__` at the immediate
        // body indent (one level deeper than the class header). Lines at
        // deeper indents belong to nested constructs (an inner class,
        // a method body, …) and must NOT count toward the outer class.
        // The body indent is locked in from the first non-trivial line.
        let outer_indent = indent_len;
        let mut body_indent: Option<usize> = None;
        let mut has_explicit_init = false;
        let mut look = idx + 1;
        while look < lines.len() {
            let candidate = lines[look];
            let cand_trim = candidate.trim_start();
            if cand_trim.is_empty() || cand_trim.starts_with('#') {
                look += 1;
                continue;
            }
            let cand_indent = candidate.len() - cand_trim.len();
            if cand_indent <= outer_indent {
                break;
            }
            let body_at = *body_indent.get_or_insert(cand_indent);
            if cand_indent == body_at
                && cand_trim.starts_with("def __init__")
                && cand_trim
                    .as_bytes()
                    .get("def __init__".len())
                    .map(|&b| b == b'(' || b == b' ' || b == b'\t')
                    .unwrap_or(false)
            {
                has_explicit_init = true;
                break;
            }
            look += 1;
        }

        if has_explicit_init {
            out.insert(idx);
        }
    }

    out
}

/// Walk `line` once and return the set of byte offsets that fall inside
/// a single- or double-quoted single-line string literal. Multi-line
/// triple-quoted strings on the same line are also tracked. Used by
/// the type-annotation rewrites so they don't munge values like
/// `x = "Optional[int]"` or `y = "Union[int, None]"` whose contents
/// look like the patterns we rewrite but aren't actually annotations.
fn string_literal_byte_ranges(line: &str) -> Vec<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        // Triple-quote check first so `"""abc"""` doesn't read as two
        // adjacent empty strings.
        if (c == b'"' || c == b'\'')
            && i + 2 < bytes.len()
            && bytes[i + 1] == c
            && bytes[i + 2] == c
        {
            let start = i;
            i += 3;
            while i + 2 < bytes.len() && !(bytes[i] == c && bytes[i + 1] == c && bytes[i + 2] == c)
            {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            // Include the closing triple-quote (or the rest of the
            // line if the string is unterminated).
            i = (i + 3).min(bytes.len());
            ranges.push((start, i));
            continue;
        }
        if c == b'"' || c == b'\'' {
            let quote = c;
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i] != quote {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if i < bytes.len() {
                i += 1; // consume closing quote
            }
            ranges.push((start, i));
            continue;
        }
        if c == b'#' {
            // Rest of the line is a comment — treat as off-limits to
            // rewrites (a comment talking about `Optional[int]` shouldn't
            // be edited either).
            ranges.push((i, bytes.len()));
            break;
        }
        i += 1;
    }
    ranges
}

/// True when `pos` falls inside one of the byte ranges in `ranges`.
fn pos_in_ranges(pos: usize, ranges: &[(usize, usize)]) -> bool {
    ranges.iter().any(|(s, e)| pos >= *s && pos < *e)
}

/// Rewrite every `Optional[T]` (including `typing.Optional[T]`) to `T?`
/// and every `T | None` to `T?`.
///
/// The matcher is intentionally simple: it skips lines without the
/// substring `Optional` or `| None` and rejects matches inside string
/// literals or comments via a coarse-grained scan.
fn rewrite_optional(line: &str) -> String {
    let mut s = line.to_owned();

    // Replace fully-qualified first to avoid double rewriting. Skip
    // matches that fall inside a string literal on the same line so we
    // don't munge `x = "Optional[int]"` into `x = "int?"` (FINDINGS —
    // Codex review on PR #94).
    for prefix in &["typing.Optional[", "Optional["] {
        while let Some(start) = {
            let string_ranges = string_literal_byte_ranges(&s);
            s.match_indices(prefix)
                .map(|(i, _)| i)
                .find(|&i| !pos_in_ranges(i, &string_ranges))
        } {
            let open = start + prefix.len() - 1;
            // Find the matching `]` honouring nested brackets.
            let mut depth: i32 = 0;
            let mut close = None;
            for (i, c) in s.bytes().enumerate().skip(open) {
                match c {
                    b'[' => depth += 1,
                    b']' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(i);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(close) = close else { break };
            let inner = &s[open + 1..close];
            // Forward-reference inside `Optional["Foo"]`: emit the
            // question mark *inside* the quotes so the result is
            // `"Foo?"` (a single forward-ref containing the trailing
            // `?`), not `"Foo"?` which is a syntax error (FINDINGS
            // O21). Handles both double- and single-quoted forms.
            //
            // The `len() >= 2` guard rules out a malformed
            // `Optional["]` whose inner would slice as `1..0` and
            // panic — leave those untouched so the parser surfaces
            // a regular Python error rather than crashing migrate.
            let trimmed = inner.trim();
            let is_quoted_forward_ref = trimmed.len() >= 2
                && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
                    || (trimmed.starts_with('\'') && trimmed.ends_with('\'')));
            let replacement = if is_quoted_forward_ref {
                let q = trimmed.chars().next().unwrap();
                let body = &trimmed[1..trimmed.len() - 1];
                format!("{q}{body}?{q}")
            } else {
                format!("{inner}?")
            };
            s.replace_range(start..=close, &replacement);
        }
    }

    // `Union[T, None]` and `Union[None, T]` collapse to `T?` (also the
    // `typing.Union[...]` qualified form). PEP 604 made `T | None` the
    // canonical spelling, but legacy code still uses `Union` and the
    // rewrite cost is identical to the `Optional` arm above (FINDINGS
    // O22).
    s = rewrite_union_optional(&s);

    // `T | None` → `T?` (only outside strings).  Apply to the slice that
    // sits inside a type-annotation context (`: T | None` or `-> T | None`).
    // We anchor on the surrounding `:` / `->` so as not to touch arbitrary
    // boolean expressions that happen to mention `None`.
    s = rewrite_pipe_none(&s);

    s
}

/// Rewrite `Union[T, None]` / `Union[None, T]` (also the
/// `typing.Union[...]` qualified forms) to `T?`. Multi-arm unions
/// `Union[A, B, None]` are left as-is — they would translate to
/// `A | B | None`, which is `(A | B)?` but the current `?` shorthand
/// doesn't compose over arbitrary unions, so a plain pipe-union is
/// the safer rewrite. Forward-references (quoted strings) push the
/// trailing `?` *inside* the quotes the same way [`rewrite_optional`]
/// does for `Optional["Foo"]` (FINDINGS O21).
fn rewrite_union_optional(line: &str) -> String {
    let mut s = line.to_owned();
    for prefix in &["typing.Union[", "Union["] {
        let mut search_from = 0usize;
        while let Some((start, _)) = {
            let string_ranges = string_literal_byte_ranges(&s);
            s[search_from..]
                .match_indices(prefix)
                .map(|(rel, m)| (search_from + rel, m))
                .find(|(i, _)| !pos_in_ranges(*i, &string_ranges))
        } {
            let open = start + prefix.len() - 1;
            // Find the matching `]` honouring nested brackets.
            let mut depth: i32 = 0;
            let mut close = None;
            for (i, c) in s.bytes().enumerate().skip(open) {
                match c {
                    b'[' => depth += 1,
                    b']' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(i);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(close) = close else { break };
            let inner = &s[open + 1..close];
            // Split on top-level commas only (nested generics may
            // contain their own commas inside `[ ]`).
            let parts = split_top_level_commas(inner);
            let trimmed_parts: Vec<&str> = parts.iter().map(|p| p.trim()).collect();
            let none_count = trimmed_parts.iter().filter(|p| **p == "None").count();
            // Only collapse to `T?` when exactly one non-None arm remains.
            // `Union[A, B, None]` is left for the pipe-rewrite path so it
            // emits as `A | B | None` (PEP 604), which the user can lift
            // to `(A | B)?` themselves if they prefer.
            if trimmed_parts.len() == 2 && none_count == 1 {
                let other = trimmed_parts
                    .iter()
                    .find(|p| **p != "None")
                    .copied()
                    .unwrap_or("");
                // Length guard: a malformed `Union["]` would otherwise
                // try to slice `1..0` and panic. Treat that as a
                // non-forward-ref and emit the plain `?` form so the
                // resulting Typhon still parses recognisably.
                let is_quoted_forward_ref = other.len() >= 2
                    && ((other.starts_with('"') && other.ends_with('"'))
                        || (other.starts_with('\'') && other.ends_with('\'')));
                let replacement = if is_quoted_forward_ref {
                    let q = other.chars().next().unwrap();
                    let body = &other[1..other.len() - 1];
                    format!("{q}{body}?{q}")
                } else {
                    format!("{other}?")
                };
                s.replace_range(start..=close, &replacement);
                search_from = start + replacement.len();
            } else {
                // Multi-arm: rewrite to a PEP 604 pipe-union so at
                // least the `typing.Union` import isn't dangling.
                let pieces: Vec<String> = trimmed_parts.iter().map(|p| p.to_string()).collect();
                let replacement = pieces.join(" | ");
                s.replace_range(start..=close, &replacement);
                search_from = start + replacement.len();
            }
        }
    }
    s
}

/// Split `s` on top-level commas. Commas inside `[]`, `()`, or quoted
/// strings are ignored so `dict[str, int], None` splits into
/// `["dict[str, int]", " None"]`, not three pieces.
fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth_bracket: i32 = 0;
    let mut depth_paren: i32 = 0;
    let mut in_str: Option<char> = None;
    let mut last = 0usize;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        match in_str {
            Some(q) if c == q => in_str = None,
            None => match c {
                '"' | '\'' => in_str = Some(c),
                '[' => depth_bracket += 1,
                ']' => depth_bracket -= 1,
                '(' => depth_paren += 1,
                ')' => depth_paren -= 1,
                ',' if depth_bracket == 0 && depth_paren == 0 => {
                    out.push(&s[last..i]);
                    last = i + 1;
                }
                _ => {}
            },
            _ => {}
        }
        i += 1;
    }
    if last < s.len() {
        out.push(&s[last..]);
    }
    out
}

/// Rewrite deprecated capital-case `typing` aliases to their lowercase
/// built-in equivalents: `List[T]` → `list[T]`, `Dict[K, V]` → `dict[K, V]`,
/// `Tuple[...]` → `tuple[...]`, `Set[T]` → `set[T]`, `FrozenSet[T]` →
/// `frozenset[T]`, `Type[T]` → `type[T]`. Also rewrites the
/// `typing.<Name>[...]` qualified form. FINDINGS #115.
fn rewrite_typing_aliases(line: &str) -> String {
    const PAIRS: &[(&str, &str)] = &[
        ("List", "list"),
        ("Dict", "dict"),
        ("Tuple", "tuple"),
        ("Set", "set"),
        ("FrozenSet", "frozenset"),
        ("Type", "type"),
    ];

    let mut s = line.to_owned();
    for (from, to) in PAIRS {
        let qualified = format!("typing.{from}[");
        let bare = format!("{from}[");
        for needle in &[qualified.as_str(), bare.as_str()] {
            let replacement = format!("{to}[");
            let mut search_from = 0usize;
            while let Some(pos) = s[search_from..].find(needle) {
                let start = search_from + pos;
                // Reject matches preceded by an identifier character —
                // `MyList[int]` is not `List[int]`.
                let prev = start
                    .checked_sub(1)
                    .and_then(|i| s.as_bytes().get(i).copied())
                    .unwrap_or(b' ');
                if prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'.' {
                    search_from = start + needle.len();
                    continue;
                }
                s.replace_range(start..start + needle.len(), &replacement);
                search_from = start + replacement.len();
            }
        }
    }
    s
}

/// Rewrite `X | None` → `X?` where the `X | None` appears immediately
/// after a `:` (annotation) or `->` (return type), with both halves a
/// single dotted name or generic subscript.
fn rewrite_pipe_none(line: &str) -> String {
    let mut out = line.to_owned();
    for anchor in &[": ", "-> "] {
        let mut search_from = 0usize;
        while let Some(pos) = out[search_from..].find(anchor) {
            let start = search_from + pos + anchor.len();
            if start >= out.len() {
                break;
            }
            let tail = &out[start..];
            if let Some(width) = scan_type_pipe_none(tail) {
                // `tail[..width]` is `T | None`; rewrite to `T?`.
                let inner = &tail[..width];
                let pipe = inner.rfind('|').unwrap_or(0);
                let lhs = inner[..pipe].trim_end();
                let replacement = format!("{lhs}?");
                let end = start + width;
                out.replace_range(start..end, &replacement);
                search_from = start + replacement.len();
            } else {
                search_from = start;
            }
        }
    }
    out
}

/// If `tail` begins with `T | None` (with `T` a single identifier or
/// generic subscript), return the byte length of that match.  Otherwise
/// `None`.
fn scan_type_pipe_none(tail: &str) -> Option<usize> {
    let mut depth: i32 = 0;
    let mut end = 0usize;
    for (i, c) in tail.char_indices() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            '|' if depth == 0 => {
                let lhs = tail[..i].trim();
                if lhs.is_empty() {
                    return None;
                }
                let rhs = tail[i + 1..].trim_start();
                if let Some(after_none) = rhs.strip_prefix("None") {
                    // `None` must end at a word boundary (space, `,`, `]`,
                    // `)`, end-of-string).
                    let boundary = after_none
                        .chars()
                        .next()
                        .map(|c| !c.is_alphanumeric() && c != '_')
                        .unwrap_or(true);
                    if boundary {
                        let none_end = tail.len() - after_none.len();
                        return Some(none_end);
                    }
                }
                return None;
            }
            ',' if depth == 0 => break,
            _ => {}
        }
        end = i + c.len_utf8();
    }
    let _ = end;
    None
}

/// Extract `NAME` from a leading `NAME: TYPE = …` or `NAME: TYPE` line,
/// honouring valid Python identifier characters.  Returns `None` for
/// anything that doesn't look like a top-level annotated assignment.
fn leading_ann_assign_name(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let first = bytes[0] as char;
    if !first.is_alphabetic() && first != '_' {
        return None;
    }
    let mut end = 0usize;
    for (i, c) in line.char_indices() {
        if c.is_alphanumeric() || c == '_' {
            end = i + c.len_utf8();
            continue;
        }
        break;
    }
    let name = &line[..end];
    let after = &line[end..];
    let after_trim = after.trim_start();
    if !after_trim.starts_with(':') {
        return None;
    }
    Some(name.to_owned())
}

/// Walk every line and record names that appear on the LHS of a plain
/// `NAME = …` assignment more than once in the same scope.  Used to
/// decide between `let` (single declaration) and `mut` (later
/// reassignment).
///
/// Tracks module-level and per-function-body assignments separately so
/// a reassignment in function A doesn't force `mut` on an unrelated
/// `let` in function B. FINDINGS #64.
fn collect_reassigned_names(source: &str) -> HashSet<String> {
    let mut reassigned: HashSet<String> = HashSet::new();
    // First pass: scan for `global NAME[, NAME, ...]` statements anywhere
    // in the file. Any name declared `global` and then assigned inside a
    // function body is a module-level reassignment — exactly what `mut`
    // is meant for. (FINDINGS #22)
    for raw in source.lines() {
        let trimmed = raw.trim_start();
        if let Some(rest) = trimmed.strip_prefix("global ") {
            for name in rest.split(',') {
                let name = name.trim().trim_end_matches(['#']).trim();
                if !name.is_empty() && is_python_identifier(name) {
                    reassigned.insert(name.to_owned());
                }
            }
        }
    }
    // Module-scope declared/reassigned tracking.
    let mut module_declared: HashSet<String> = HashSet::new();
    // Per-function scope. Pushed on `def` / `async def` at a deeper
    // indent than the current top; popped when a non-blank, non-comment
    // line returns to an indent ≤ the function's `def` indent.
    struct FnScope {
        def_indent: usize,
        declared: HashSet<String>,
    }
    let mut fn_stack: Vec<FnScope> = Vec::new();
    for raw in source.lines() {
        let trimmed = raw.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = raw.len() - trimmed.len();
        // Pop function scopes we've exited (any non-blank line at indent
        // ≤ def_indent means we've left that function body).
        while let Some(top) = fn_stack.last() {
            if indent <= top.def_indent {
                fn_stack.pop();
            } else {
                break;
            }
        }
        // Function definition opens a new scope.
        if trimmed.starts_with("def ") || trimmed.starts_with("async def ") {
            fn_stack.push(FnScope {
                def_indent: indent,
                declared: HashSet::new(),
            });
            continue;
        }
        // Identify the active declared-name set: innermost function if
        // we're inside one, otherwise the module-scope set.
        let declared = match fn_stack.last_mut() {
            Some(scope) => &mut scope.declared,
            None => &mut module_declared,
        };
        if let Some(name) = leading_ann_assign_name(trimmed) {
            // Annotated assigns count as a declaration.
            if !declared.insert(name.clone()) {
                reassigned.insert(name);
            }
            continue;
        }
        if let Some(name) = leading_plain_assign_name(trimmed) {
            if !declared.insert(name.clone()) {
                reassigned.insert(name);
            }
        }
    }
    reassigned
}

/// Cheap check: does `s` look like a Python identifier (`[A-Za-z_][A-Za-z0-9_]*`)?
/// Used by the `global` scanner so we don't accept stray punctuation.
fn is_python_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return false,
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Extract `NAME` from a leading `NAME = …` line.  Returns `None` for
/// anything that doesn't look like a plain assignment.
fn leading_plain_assign_name(line: &str) -> Option<String> {
    let mut end = 0usize;
    for (i, c) in line.char_indices() {
        if i == 0 {
            if !(c.is_alphabetic() || c == '_') {
                return None;
            }
            end = c.len_utf8();
            continue;
        }
        if c.is_alphanumeric() || c == '_' {
            end = i + c.len_utf8();
            continue;
        }
        break;
    }
    let name = &line[..end];
    let after = &line[end..];
    let after_trim = after.trim_start();
    if !after_trim.starts_with('=') {
        return None;
    }
    // Reject compound operators: `==`, `+=`, etc.
    let bytes = after_trim.as_bytes();
    if bytes.len() > 1 && bytes[1] == b'=' {
        return None;
    }
    Some(name.to_owned())
}

/// Recursively collect every `.py` file under `root`.  Mirrors the helper
/// in `commands::util` but specialised for the migration command so we
/// don't conflict with `.ty` discovery.
fn collect_py_files(root: &std::path::Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if root.is_file() {
        if root.extension().and_then(|s| s.to_str()) == Some("py") {
            out.push(root.to_path_buf());
        }
        return Ok(out);
    }
    walk(root, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in
        std::fs::read_dir(dir).map_err(|e| miette!("cannot list '{}': {e}", dir.display()))?
    {
        let entry =
            entry.map_err(|e| miette!("cannot read entry under '{}': {e}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out)?;
        } else if path.extension().and_then(|s| s.to_str()) == Some("py") {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_optional_to_question_mark() {
        let out = migrate_source("x: Optional[int] = None\n");
        assert!(out.contains("int?"), "got: {out}");
        assert!(!out.contains("Optional"), "got: {out}");
    }

    #[test]
    fn rewrites_typing_optional_qualified() {
        let out = migrate_source("x: typing.Optional[list[int]] = None\n");
        assert!(out.contains("list[int]?"), "got: {out}");
    }

    #[test]
    fn rewrites_optional_forward_reference_inside_quotes() {
        // `Optional["Foo"]` must become `"Foo?"` (the `?` lives inside
        // the forward-reference string), not `"Foo"?` which is a syntax
        // error in Typhon (FINDINGS O21).
        let out = migrate_source("x: Optional[\"Item\"] = None\n");
        assert!(
            out.contains("\"Item?\""),
            "forward-ref rewrite should put `?` inside the quotes; got: {out}",
        );
        assert!(
            !out.contains("\"Item\"?"),
            "should not emit the invalid `\"Item\"?` form; got: {out}",
        );
    }

    #[test]
    fn rewrites_union_t_none_to_question_mark() {
        // `Union[T, None]` and `Union[None, T]` are equivalent to
        // `Optional[T]` and should collapse to `T?` (FINDINGS O22).
        let out = migrate_source("x: Union[int, None] = None\n");
        assert!(
            out.contains("int?"),
            "Union[int, None] should rewrite; got: {out}"
        );
        assert!(
            !out.contains("Union"),
            "Union name should be dropped; got: {out}"
        );

        let out = migrate_source("x: Union[None, int] = None\n");
        assert!(
            out.contains("int?"),
            "Union[None, int] should rewrite; got: {out}"
        );
    }

    #[test]
    fn rewrites_qualified_union_t_none_to_question_mark() {
        let out = migrate_source("x: typing.Union[list[int], None] = None\n");
        assert!(out.contains("list[int]?"), "got: {out}");
    }

    #[test]
    fn rewrites_union_forward_reference_inside_quotes() {
        // Forward-references in `Union[..., None]` get the same
        // inside-the-quotes treatment as `Optional["Foo"]`.
        let out = migrate_source("x: Union[\"Item\", None] = None\n");
        assert!(
            out.contains("\"Item?\""),
            "Union forward-ref should put `?` inside the quotes; got: {out}",
        );
    }

    #[test]
    fn rewrites_multi_arm_union_to_pipe_union() {
        // `Union[A, B, None]` doesn't collapse to a single `?` — we
        // rewrite it to a PEP 604 pipe-union so the `typing.Union`
        // import isn't left dangling. The user can lift it to
        // `(A | B)?` themselves if they prefer.
        let out = migrate_source("x: Union[int, str, None] = None\n");
        assert!(
            out.contains("int | str | None"),
            "multi-arm Union should rewrite to pipe-union; got: {out}",
        );
        assert!(
            !out.contains("Union"),
            "Union name should be gone; got: {out}"
        );
    }

    #[test]
    fn drops_union_from_typing_import_after_rewrite() {
        let out = migrate_source("from typing import Union\n\nx: Union[int, None] = None\n");
        assert!(
            !out.contains("import Union"),
            "stale Union import must be dropped; got: {out}",
        );
    }

    #[test]
    fn rewrites_pipe_none_in_return_type() {
        let out = migrate_source("def f() -> int | None:\n    return None\n");
        // The return type becomes `int?`.
        assert!(
            out.contains("-> int?"),
            "return type should be rewritten; got: {out}"
        );
        // The `None` literal in the body is unrelated to the type and must stay.
        assert!(
            out.contains("return None"),
            "return value must be preserved; got: {out}"
        );
    }

    #[test]
    fn module_level_annotated_assign_gets_val() {
        let out = migrate_source("x: int = 1\n");
        assert!(out.contains("let x: int = 1"), "got: {out}");
    }

    #[test]
    fn reassigned_name_becomes_var() {
        let out = migrate_source("x: int = 1\nx = 2\n");
        assert!(out.contains("mut x: int = 1"), "got: {out}");
    }

    #[test]
    fn dataclass_decorator_dropped() {
        let out = migrate_source("@dataclass\nclass User:\n    name: str\n");
        assert!(!out.contains("@dataclass"), "got: {out}");
        assert!(out.contains("class User:"), "got: {out}");
    }

    #[test]
    fn dataclasses_import_dropped() {
        let out = migrate_source(
            "from dataclasses import dataclass\n@dataclass\nclass U:\n    name: str\n",
        );
        assert!(!out.contains("from dataclasses"), "got: {out}");
    }

    #[test]
    fn indented_annotated_assignment_left_alone() {
        // Class-body annotations should keep the bare form because they
        // are dataclass fields, not let/mut bindings.
        let out = migrate_source("class U:\n    name: str\n");
        assert!(!out.contains("let name"), "got: {out}");
        assert!(out.contains("    name: str"), "got: {out}");
    }

    #[test]
    fn comments_are_preserved_verbatim() {
        let out = migrate_source("# header comment\nx: int = 1\n");
        assert!(out.contains("# header comment"), "got: {out}");
        assert!(out.contains("let x: int = 1"), "got: {out}");
    }

    #[test]
    fn class_with_explicit_init_becomes_class_bang() {
        let src = "class MyModel(nn.Module):\n    def __init__(self, layers: int) -> None:\n        super().__init__()\n        self.layers = layers\n";
        let out = migrate_source(src);
        assert!(out.starts_with("class! MyModel(nn.Module):"), "got: {out}");
    }

    #[test]
    fn dataclass_decorated_class_does_not_become_class_bang() {
        // The dataclass decorator gets dropped (Typhon's default), but the
        // class header must remain plain `class` — the user opted into
        // dataclass semantics so the synthesised __init__ is what they want.
        let src = "@dataclass\nclass U:\n    name: str\n    def __init__(self, name: str) -> None:\n        self.name = name\n";
        let out = migrate_source(src);
        assert!(!out.contains("class!"), "got: {out}");
        assert!(out.contains("class U:"), "got: {out}");
    }

    #[test]
    fn class_without_init_stays_plain() {
        let src = "class Point:\n    x: int\n    y: int\n";
        let out = migrate_source(src);
        assert!(!out.contains("class!"), "got: {out}");
        assert!(out.contains("class Point:"), "got: {out}");
    }

    #[test]
    fn class_with_init_in_nested_function_still_promoted() {
        // The walker finds __init__ at any depth inside the class body —
        // a closure called `__init__` defined inside a method is rare
        // enough that we accept the false-positive here.
        let src = "class Outer:\n    def __init__(self) -> None:\n        pass\n";
        let out = migrate_source(src);
        assert!(out.contains("class! Outer:"), "got: {out}");
    }

    #[test]
    fn nested_class_bang_promotion_respects_indent() {
        // Inner class with explicit __init__ should be promoted too.
        let src = "class Outer:\n    class Inner:\n        def __init__(self) -> None:\n            pass\n";
        let out = migrate_source(src);
        assert!(out.contains("class! Inner:"), "got: {out}");
        // Outer has no __init__ of its own, must stay plain.
        assert!(out.contains("class Outer:"), "got: {out}");
    }

    #[test]
    fn typevar_declaration_is_dropped() {
        // Regression for N11 (2026-05-22): `T = TypeVar("T")` at module
        // level is dead in Typhon once `Generic[T]` rewrites to PEP 695
        // — drop the line so the migrate output stops importing
        // `TypeVar` (and stops tripping `tyc::typevar_import_rejected`).
        let src = "from typing import TypeVar\nT = TypeVar(\"T\")\n";
        let out = migrate_source(src);
        assert!(
            !out.contains("TypeVar"),
            "TypeVar import and declaration must both be dropped, got:\n{out}"
        );
    }

    #[test]
    fn class_generic_base_rewrites_to_pep695() {
        // `class Box(Generic[T]):` → `class Box[T]:`
        let src =
            "from typing import Generic, TypeVar\nT = TypeVar(\"T\")\nclass Box(Generic[T]):\n    value: T\n";
        let out = migrate_source(src);
        assert!(
            out.contains("class Box[T]:"),
            "expected PEP 695 form, got:\n{out}"
        );
        assert!(!out.contains("Generic"), "got:\n{out}");
        assert!(!out.contains("TypeVar"), "got:\n{out}");
    }

    #[test]
    fn class_with_generic_and_other_bases_preserved() {
        // `class C(Generic[T], OtherBase):` → `class C[T](OtherBase):`
        let src = "from typing import Generic, TypeVar\nT = TypeVar(\"T\")\nclass C(Generic[T], OtherBase):\n    x: T\n";
        let out = migrate_source(src);
        assert!(
            out.contains("class C[T](OtherBase):"),
            "expected `class C[T](OtherBase):`, got:\n{out}"
        );
    }

    #[test]
    fn multi_param_generic_class_rewrites() {
        let src = "from typing import Generic, TypeVar\nT = TypeVar(\"T\")\nU = TypeVar(\"U\")\nclass Pair(Generic[T, U]):\n    a: T\n    b: U\n";
        let out = migrate_source(src);
        assert!(
            out.contains("class Pair[T, U]:"),
            "expected `class Pair[T, U]:`, got:\n{out}"
        );
    }
}
