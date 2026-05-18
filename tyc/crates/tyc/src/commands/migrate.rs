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

    let mut out = String::with_capacity(source.len());
    for (line_index, line) in source.split_inclusive('\n').enumerate() {
        let raw = line.trim_end_matches(['\n', '\r']);
        let terminator = &line[raw.len()..];

        let rewritten = if raw.trim().is_empty() {
            raw.to_owned()
        } else {
            rewrite_line(raw, &reassigned, line_index, &bang_class_lines)
        };

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
    }

    format!("{indent}{body}")
}

/// Rewrite `from typing import …, Optional, …` by dropping the `Optional`
/// name (now unused after Rule 1). Returns `Some(rewritten)` when the
/// line matched; `Some("")` signals the caller should drop the line
/// entirely (the import would otherwise be empty). Returns `None` when
/// the line wasn't a `from typing import …` form, so the caller can
/// continue with other rewrites.
///
/// Conservatively skips wildcard (`*`) and `as`-aliased imports so we
/// never silently drop a renamed `Optional` the user may use elsewhere.
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
    if !names.contains(&"Optional") {
        return None;
    }
    let kept: Vec<&str> = names.iter().copied().filter(|n| *n != "Optional").collect();
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

/// Rewrite every `Optional[T]` (including `typing.Optional[T]`) to `T?`
/// and every `T | None` to `T?`.
///
/// The matcher is intentionally simple: it skips lines without the
/// substring `Optional` or `| None` and rejects matches inside string
/// literals or comments via a coarse-grained scan.
fn rewrite_optional(line: &str) -> String {
    let mut s = line.to_owned();

    // Replace fully-qualified first to avoid double rewriting.
    for prefix in &["typing.Optional[", "Optional["] {
        while let Some(start) = s.find(prefix) {
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
            let replacement = format!("{inner}?");
            s.replace_range(start..=close, &replacement);
        }
    }

    // `T | None` → `T?` (only outside strings).  Apply to the slice that
    // sits inside a type-annotation context (`: T | None` or `-> T | None`).
    // We anchor on the surrounding `:` / `->` so as not to touch arbitrary
    // boolean expressions that happen to mention `None`.
    s = rewrite_pipe_none(&s);

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
/// `NAME = …` assignment (no annotation).  Used to decide between `val`
/// (single declaration) and `var` (later reassignment).
fn collect_reassigned_names(source: &str) -> HashSet<String> {
    let mut declared: HashSet<String> = HashSet::new();
    let mut reassigned: HashSet<String> = HashSet::new();
    // First pass: scan for `global NAME[, NAME, ...]` statements anywhere
    // in the file. Any name declared `global` and then assigned inside a
    // function body is a module-level reassignment — exactly what `mut`
    // is meant for. The original migrator only looked at top-level
    // assignments and so missed counter/accumulator patterns lifted via
    // `global`. (FINDINGS #22)
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
    for raw in source.lines() {
        let trimmed = raw.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Skip indented lines (only top-level matters for the val/var
        // distinction we apply).
        if raw.len() != trimmed.len() {
            continue;
        }
        if let Some(name) = leading_ann_assign_name(trimmed) {
            declared.insert(name);
            continue;
        }
        // Plain `NAME = expr` (no annotation): if the name was already
        // declared, this is a reassignment.
        if let Some(name) = leading_plain_assign_name(trimmed) {
            if declared.contains(&name) {
                reassigned.insert(name);
            } else {
                declared.insert(name.clone());
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
}
