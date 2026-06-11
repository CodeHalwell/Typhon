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
//! 7. `@dataclass(frozen=True)` / `@dataclasses.dataclass(frozen=True)`
//!    decorators are dropped and the class header gains a trailing
//!    `frozen` modifier (`class Vec frozen:`). Mixed-keyword decorators
//!    like `@dataclass(frozen=True, slots=True)` are also recognised.
//! 8. `class X(Protocol):` is rewritten to `interface X:`. The lone
//!    `Protocol` base is dropped because Typhon's `interface` keyword
//!    already emits `class X(Protocol):` at desugar time. Multi-base
//!    forms (`class X(Protocol, Foo):`) are left untouched because
//!    they need manual review. `Protocol` is also stripped from any
//!    `from typing import …` line.
//! 9. `NAME = NewType("NAME", BASE)` at module level becomes
//!    `newtype NAME = BASE`. The matching `from typing import NewType`
//!    entry is dropped.
//!
//! Output is written next to the input with the `.ty` extension; `--check`
//! emits to stdout without touching the disk so the user can preview.

use std::collections::{HashMap, HashSet};
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
    let mut bang_class_lines = collect_bang_class_lines(source);
    let frozen_class_lines = collect_frozen_class_lines(source);
    let frozen_decorator_lines = collect_frozen_decorator_lines(source);

    // FINDINGS #33: identify classes whose hand-rolled `__init__` body is
    // exactly `self.field = field` for each parameter. The migrator can
    // safely strip such an `__init__` and let Typhon's auto-`__init__`
    // synthesis take over — the result is a plain `class` (NOT `class!`)
    // with field declarations matching the parameter types. Without this
    // pass the textual rewriter promotes any class with an `__init__`
    // to `class!`, which then fails `tyc check` with `manual_init` if the
    // user re-runs the migrated file. The function returns:
    //   1. `trivial`: line indices of class headers whose init is trivial.
    //   2. `skip`: line indices to drop verbatim (the `__init__` body).
    //   3. `inject`: per-class-header field declarations to append at the
    //      end of the (otherwise unchanged) class body.
    let (trivial_init_classes, skip_lines, injected_fields) = collect_trivial_init_classes(source);
    // Pull these classes out of `bang_class_lines` so the textual
    // rewriter leaves them as plain `class …:` instead of bumping them
    // to `class! …:` (the dataclass-style default is what we want).
    for line in &trivial_init_classes {
        bang_class_lines.remove(line);
    }

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

        // FINDINGS #33: drop lines belonging to a stripped trivial `__init__`.
        // Skipping the whole line (including its trailing newline) keeps
        // the byte-level output tidy — no blank-line clusters where the
        // method used to live.
        if skip_lines.contains(&line_index) {
            continue;
        }

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
                &frozen_class_lines,
                &frozen_decorator_lines,
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

        // FINDINGS #33: when this line opened a trivial-init class,
        // emit the synthesised field declarations immediately after it
        // so the rewritten output reads like a normal dataclass-style
        // body. Indent matches the class header (header indent + 4),
        // covering both top-level and nested class declarations.
        if let Some(fields) = injected_fields.get(&line_index) {
            let body_indent = " ".repeat(indent + 4);
            for (name, ty) in fields {
                out.push_str(&body_indent);
                out.push_str(name);
                out.push_str(": ");
                out.push_str(ty);
                out.push_str(terminator);
            }
        }
    }

    // Post-passes over the line-rewritten output: modernise `class X(Enum):`
    // to the `enum` keyword, simplify `field(default_factory=...)` to
    // Typhon's bare-literal sugar, relocate class-body methods into `impl`
    // blocks (Rule 4 — previously the migrator's own checker immediately
    // warned about its output), and drop imports the rewrites orphaned.
    let out = rewrite_enum_classes(&out);
    let out = simplify_field_default_factories(&out);
    let out = move_methods_to_impl(&out);
    prune_stale_migration_imports(&out)
}

/// `class X(Enum):` / `class X(enum.Enum):` → `enum X:`. Only the bare
/// `Enum` base — `IntEnum` / `StrEnum` / `Flag` mixins have no `enum`
/// keyword spelling and keep their class form (the auto-skip emit path
/// handles them).
fn rewrite_enum_classes(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    for line in source.split_inclusive('\n') {
        let raw = line.trim_end_matches(['\n', '\r']);
        let terminator = &line[raw.len()..];
        let trimmed = raw.trim_start();
        let indent = &raw[..raw.len() - trimmed.len()];
        let rewritten = (|| -> Option<String> {
            let rest = trimmed.strip_prefix("class ")?;
            let open = rest.find('(')?;
            let name = rest[..open].trim();
            if name.is_empty() || !is_python_identifier(name) {
                return None;
            }
            let close = rest.find(')')?;
            let bases = rest[open + 1..close].trim();
            if bases != "Enum" && bases != "enum.Enum" {
                return None;
            }
            if !rest[close + 1..].trim_start().starts_with(':') {
                return None;
            }
            Some(format!("{indent}enum {name}:"))
        })();
        match rewritten {
            Some(r) => {
                out.push_str(&r);
                out.push_str(terminator);
            }
            None => out.push_str(line),
        }
    }
    out
}

/// `name: T = field(default_factory=list)` → `name: T = []` (and dict /
/// set). Typhon's desugar re-creates the per-instance factory from the
/// bare literal, so the sugar form is canonical.
fn simplify_field_default_factories(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    for line in source.split_inclusive('\n') {
        let raw = line.trim_end_matches(['\n', '\r']);
        let terminator = &line[raw.len()..];
        let rewritten = (|| -> Option<String> {
            let eq = raw.find(" = field(default_factory=")?;
            let tail = &raw[eq + " = field(default_factory=".len()..];
            let (literal, rest) = if let Some(r) = tail.strip_prefix("list)") {
                ("[]", r)
            } else if let Some(r) = tail.strip_prefix("dict)") {
                ("{}", r)
            } else if let Some(r) = tail.strip_prefix("set)") {
                ("set()", r)
            } else {
                return None;
            };
            if !rest.trim().is_empty() {
                return None;
            }
            Some(format!("{} = {}", &raw[..eq], literal))
        })();
        match rewritten {
            Some(r) => {
                out.push_str(&r);
                out.push_str(terminator);
            }
            None => out.push_str(line),
        }
    }
    out
}

/// Relocate method definitions out of top-level `class` bodies into an
/// `impl ClassName:` block emitted right after the class — Typhon's Rule
/// 4 (`tyc::method_in_class_body` is a warning on exactly the output the
/// migrator used to produce). Skips `enum` / `interface` / `plain class`
/// blocks (methods belong in their bodies or have different semantics)
/// and keeps a hand-written `__init__` inside `class!` bodies (where it
/// is meaningful). Only top-level classes are transformed.
fn move_methods_to_impl(source: &str) -> String {
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0usize;
    while i < lines.len() {
        let raw = lines[i].trim_end_matches(['\n', '\r']);
        let trimmed = raw.trim_start();
        let indent = raw.len() - trimmed.len();
        let is_class_header = indent == 0
            && (trimmed.starts_with("class ") || trimmed.starts_with("class! "))
            && trimmed.ends_with(':');
        if !is_class_header {
            out.push_str(lines[i]);
            i += 1;
            continue;
        }
        let is_bang = trimmed.starts_with("class! ");
        let name_part = trimmed
            .strip_prefix("class! ")
            .or_else(|| trimmed.strip_prefix("class "))
            .unwrap_or(trimmed);
        let name_end = name_part
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(name_part.len());
        let class_name = &name_part[..name_end];
        if class_name.is_empty() {
            out.push_str(lines[i]);
            i += 1;
            continue;
        }
        // Block extent: lines until the next non-blank line at indent 0.
        let mut end = i + 1;
        while end < lines.len() {
            let r = lines[end].trim_end_matches(['\n', '\r']);
            let t = r.trim_start();
            if !t.is_empty() && r.len() - t.len() == 0 {
                break;
            }
            end += 1;
        }
        // Partition body items at indent 4 into methods vs the rest.
        let mut others: Vec<&str> = Vec::new();
        let mut methods: Vec<&str> = Vec::new();
        let mut j = i + 1;
        while j < end {
            let r = lines[j].trim_end_matches(['\n', '\r']);
            let t = r.trim_start();
            let ind = r.len() - t.len();
            if t.is_empty() {
                // Blank lines attach to whatever item follows; buffer by
                // peeking at the next structural line.
                let mut k = j + 1;
                while k < end {
                    let rk = lines[k].trim_end_matches(['\n', '\r']);
                    if !rk.trim().is_empty() {
                        break;
                    }
                    k += 1;
                }
                let next_is_method = k < end && {
                    let rk = lines[k].trim_end_matches(['\n', '\r']);
                    let tk = rk.trim_start();
                    rk.len() - tk.len() == 4
                        && (tk.starts_with("def ")
                            || tk.starts_with("async def ")
                            || tk.starts_with('@'))
                };
                for blank in &lines[j..k.min(end)] {
                    if next_is_method {
                        methods.push(blank);
                    } else {
                        others.push(blank);
                    }
                }
                j = k;
                continue;
            }
            if ind == 4 && (t.starts_with("def ") || t.starts_with("async def ") || t.starts_with('@')) {
                // Item extent: this line plus everything indented deeper
                // (decorators chain through subsequent indent-4 def lines).
                let start_item = j;
                j += 1;
                while j < end {
                    let r2 = lines[j].trim_end_matches(['\n', '\r']);
                    let t2 = r2.trim_start();
                    let ind2 = r2.len() - t2.len();
                    if t2.is_empty() {
                        // Blank inside an item only if deeper content follows.
                        let mut k = j + 1;
                        while k < end && lines[k].trim().is_empty() {
                            k += 1;
                        }
                        let deeper_follows = k < end && {
                            let rk = lines[k].trim_end_matches(['\n', '\r']);
                            let tk = rk.trim_start();
                            rk.len() - tk.len() > 4
                        };
                        if deeper_follows {
                            j = k;
                            continue;
                        }
                        break;
                    }
                    if ind2 <= 4 && !(ind2 == 4 && t.starts_with('@') && (t2.starts_with("def ") || t2.starts_with("async def ") || t2.starts_with('@'))) {
                        // A decorator item continues through the def it
                        // decorates; anything else at indent <= 4 ends it.
                        if ind2 == 4 && (t2.starts_with("def ") || t2.starts_with("async def ")) && t.starts_with('@') {
                            // covered above
                        }
                        break;
                    }
                    j += 1;
                }
                // Extend through the decorated def's body when the item
                // started at a decorator.
                let item = &lines[start_item..j];
                let is_init = item.iter().any(|l| {
                    let t = l.trim_start();
                    t.starts_with("def __init__(") || t.starts_with("async def __init__(")
                });
                if is_bang && is_init {
                    others.extend_from_slice(item);
                } else {
                    methods.extend_from_slice(item);
                }
                continue;
            }
            // Non-method item: extent = this line + deeper lines.
            others.push(lines[j]);
            j += 1;
            while j < end {
                let r2 = lines[j].trim_end_matches(['\n', '\r']);
                let t2 = r2.trim_start();
                if t2.is_empty() || r2.len() - t2.len() > 4 {
                    others.push(lines[j]);
                    j += 1;
                } else {
                    break;
                }
            }
        }
        if methods.is_empty() {
            for l in &lines[i..end] {
                out.push_str(l);
            }
            i = end;
            continue;
        }
        // Re-emit: class header + non-method body (or `pass`), then the
        // impl block carrying the methods.
        out.push_str(lines[i]);
        let mut others_trimmed: Vec<&str> = others.clone();
        while others_trimmed
            .last()
            .is_some_and(|l| l.trim().is_empty())
        {
            others_trimmed.pop();
        }
        if others_trimmed.is_empty() {
            out.push_str("    pass\n");
        } else {
            for l in &others_trimmed {
                out.push_str(l);
            }
        }
        out.push('\n');
        out.push_str(&format!("impl {class_name}:\n"));
        let mut methods_trimmed: Vec<&str> = methods.clone();
        while methods_trimmed
            .first()
            .is_some_and(|l| l.trim().is_empty())
        {
            methods_trimmed.remove(0);
        }
        while methods_trimmed
            .last()
            .is_some_and(|l| l.trim().is_empty())
        {
            methods_trimmed.pop();
        }
        let mut prev_blank = false;
        for l in &methods_trimmed {
            if l.trim().is_empty() && prev_blank {
                continue;
            }
            prev_blank = l.trim().is_empty();
            out.push_str(l);
        }
        if !out.ends_with('\n') {
            out.push('\n');
        }
        // One blank line after the impl block so the next top-level
        // definition doesn't butt against it.
        if end < lines.len() && !lines[end].trim().is_empty() {
            out.push('\n');
        }
        i = end;
    }
    out
}

/// Drop import names the earlier rewrites orphaned: `dataclass` / `field`
/// from `from dataclasses import ...` and `Enum` from `from enum import
/// ...` when the name no longer appears anywhere else in the output.
fn prune_stale_migration_imports(source: &str) -> String {
    fn referenced_outside_imports(source: &str, name: &str) -> bool {
        for line in source.lines() {
            let t = line.trim_start();
            if t.starts_with("from ") || t.starts_with("import ") {
                continue;
            }
            let mut rest = line;
            while let Some(pos) = rest.find(name) {
                let before_ok = pos == 0
                    || !rest[..pos]
                        .chars()
                        .next_back()
                        .is_some_and(|c| c.is_alphanumeric() || c == '_');
                let after = &rest[pos + name.len()..];
                let after_ok = !after
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_');
                if before_ok && after_ok {
                    return true;
                }
                rest = &rest[pos + name.len()..];
            }
        }
        false
    }
    let mut out = String::with_capacity(source.len());
    for line in source.split_inclusive('\n') {
        let raw = line.trim_end_matches(['\n', '\r']);
        let terminator = &line[raw.len()..];
        let trimmed = raw.trim_start();
        let module = if trimmed.starts_with("from dataclasses import ") {
            Some("dataclasses")
        } else if trimmed.starts_with("from enum import ") {
            Some("enum")
        } else {
            None
        };
        let Some(module) = module else {
            out.push_str(line);
            continue;
        };
        let prefix_len = format!("from {module} import ").len();
        let names: Vec<&str> = trimmed[prefix_len..]
            .split(',')
            .map(|n| n.trim())
            .filter(|n| !n.is_empty())
            .collect();
        let kept: Vec<&str> = names
            .iter()
            .copied()
            .filter(|n| referenced_outside_imports(source, n))
            .collect();
        if kept.is_empty() {
            continue; // whole import is dead
        }
        if kept.len() == names.len() {
            out.push_str(line);
            continue;
        }
        let indent = &raw[..raw.len() - trimmed.len()];
        out.push_str(&format!(
            "{indent}from {module} import {}",
            kept.join(", ")
        ));
        out.push_str(terminator);
    }
    out
}

/// Walk every line and apply rewrite rules in order.  Returns the
/// transformed line (without its terminator).
#[allow(clippy::too_many_arguments)]
fn rewrite_line(
    line: &str,
    reassigned: &HashSet<String>,
    line_index: usize,
    bang_class_lines: &HashSet<usize>,
    frozen_class_lines: &HashSet<usize>,
    frozen_decorator_lines: &HashSet<usize>,
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

    // Rule 4 / 7: drop a `@dataclass` (with or without args) decorator
    // line — also covers `@dataclass(frozen=True[, ...])` and the
    // qualified `@dataclasses.dataclass(...)` form. The frozen variant
    // has already been recorded in `frozen_decorator_lines` so the
    // class header below can append the `frozen` modifier. The
    // qualified prefix is checked first because it's a superset of the
    // bare `@dataclass` form.
    if let Some(rest) = trimmed.strip_prefix("@dataclasses.dataclass") {
        let after = rest.trim();
        if after.is_empty() || after.starts_with('(') {
            return String::new();
        }
    }
    if let Some(rest) = trimmed.strip_prefix("@dataclass") {
        let after = rest.trim();
        if after.is_empty() || after.starts_with('(') {
            return String::new();
        }
    }
    // Drop a recorded frozen decorator that didn't match the prefix
    // strip above (shouldn't happen — defensive).
    if frozen_decorator_lines.contains(&line_index) {
        return String::new();
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

    // Rule 8: rewrite `class X(Protocol):` to `interface X:`. Single-
    // base only — multi-base (Protocol + something else) needs a
    // judgement call and is left for the user.
    if body.starts_with("class ") {
        if let Some(rewritten) = rewrite_protocol_class(&body) {
            body = rewritten;
        }
    }

    // Rule 9: rewrite a module-level `X = NewType("X", Base)` to
    // `newtype X = Base`. Indented occurrences (inside class bodies or
    // function bodies) are untouched — that pattern is exotic enough
    // that automatic rewriting would surprise.
    if indent.is_empty() {
        if let Some(rewritten) = rewrite_newtype_declaration(&body) {
            body = rewritten;
        }
    }

    // Rule 7: append a trailing `frozen` modifier to a class header
    // whose `@dataclass(frozen=True[, ...])` decorator was just dropped.
    if frozen_class_lines.contains(&line_index) {
        body = append_frozen_modifier(&body);
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
    // Rules 8 / 9: Protocol → `interface`, NewType → `newtype = ` —
    // after the rewrites both names are dead imports.
    "Protocol",
    "NewType",
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
        || !lhs.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
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
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    // Find the matching `)` at depth 0.
    let bytes = rest_after_keyword.as_bytes();
    let mut depth: i32 = 0;
    let mut close: Option<usize> = None;
    for (i, b) in bytes.iter().enumerate().skip(open) {
        match *b {
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
    Some(format!(
        "{}{}{}{}",
        keyword, new_name_part, new_bases, trailer
    ))
}

/// FINDINGS #33: identify classes whose `__init__` body is just the
/// canonical `self.field = field` mirror of its parameter list — those
/// can be safely stripped because Typhon's auto-`__init__` synthesis
/// will produce the same constructor (and the resulting class stays
/// plain `class`, not `class!`). Returns:
///   - the set of *class header* line indices to demote out of
///     `bang_class_lines`,
///   - the set of *line indices to drop* verbatim (everything from the
///     `def __init__` header through the last assignment in its body),
///   - a per-class map of field declarations `(name, type)` to inject
///     into the body, in declaration order.
///
/// "Trivial" is intentionally narrow:
///   - body lines are exclusively `self.NAME = NAME` assignments,
///   - one assignment per init parameter (excluding `self`),
///   - the assignment name matches the parameter name exactly,
///   - every parameter carries a type annotation,
///   - no `super().__init__(...)`, no defaults, no `*args` / `**kwargs`,
///   - no `@dataclass` decorator on the class (handled elsewhere).
///
/// Anything more exotic stays on the `class!` path so the user keeps
/// their custom constructor — strip-and-synthesise would silently drop
/// real logic. False-negatives are preferred over false-positives.
type TrivialInitClasses = (
    HashSet<usize>,
    HashSet<usize>,
    HashMap<usize, Vec<(String, String)>>,
);

fn collect_trivial_init_classes(source: &str) -> TrivialInitClasses {
    let lines: Vec<&str> = source.lines().collect();
    let mut trivial: HashSet<usize> = HashSet::new();
    let mut skip: HashSet<usize> = HashSet::new();
    let mut inject: HashMap<usize, Vec<(String, String)>> = HashMap::new();

    for (idx, raw) in lines.iter().enumerate() {
        let trimmed = raw.trim_start();
        let is_class = trimmed.starts_with("class ") || trimmed == "class";
        if !is_class {
            continue;
        }
        let class_indent = raw.len() - trimmed.len();

        // Skip classes with a `@dataclass[(...)]` decorator on the
        // contiguous decorator stack — those already use synthesised
        // init via Python's dataclass machinery, and the migrator drops
        // the decorator while leaving the class header alone.
        let mut probe = idx;
        let mut had_dataclass_decorator = false;
        while probe > 0 {
            probe -= 1;
            let prev = lines[probe];
            let prev_trim = prev.trim_start();
            if prev_trim.is_empty() {
                continue;
            }
            let prev_indent = prev.len() - prev_trim.len();
            if prev_indent != class_indent || !prev_trim.starts_with('@') {
                break;
            }
            if let Some(after) = prev_trim.strip_prefix("@dataclass") {
                let after = after.trim();
                if after.is_empty() || after.starts_with('(') {
                    had_dataclass_decorator = true;
                    break;
                }
            }
        }
        if had_dataclass_decorator {
            continue;
        }

        // Locate the class body indent. Anything at indent <= class_indent
        // ends the class body. Search for `def __init__` at the body
        // indent only — nested `__init__`s on inner classes belong to
        // those inner classes and are handled in their own iteration.
        let mut body_indent: Option<usize> = None;
        let mut init_header: Option<usize> = None;
        let mut look = idx + 1;
        while look < lines.len() {
            let cand = lines[look];
            let cand_trim = cand.trim_start();
            if cand_trim.is_empty() || cand_trim.starts_with('#') {
                look += 1;
                continue;
            }
            let cand_indent = cand.len() - cand_trim.len();
            if cand_indent <= class_indent {
                break;
            }
            let bi = *body_indent.get_or_insert(cand_indent);
            if cand_indent == bi
                && cand_trim.starts_with("def __init__")
                && cand_trim
                    .as_bytes()
                    .get("def __init__".len())
                    .is_some_and(|&b| b == b'(' || b == b' ' || b == b'\t')
            {
                init_header = Some(look);
                break;
            }
            look += 1;
        }
        let (Some(init_line), Some(body_indent)) = (init_header, body_indent) else {
            continue;
        };

        // The `def __init__(...)` header may span multiple lines. Walk
        // forward joining lines until paren depth balances. Stop at a
        // `:` followed by EOL or a comment.
        let mut header_end = init_line;
        let mut header_buf = String::new();
        let mut depth: i32 = 0;
        let mut found_colon = false;
        while header_end < lines.len() && !found_colon {
            let l = lines[header_end];
            // Strip trailing comment for the colon scan.
            let scan = l.split('#').next().unwrap_or("");
            for b in scan.bytes() {
                match b {
                    b'(' | b'[' | b'{' => depth += 1,
                    b')' | b']' | b'}' => depth -= 1,
                    b':' if depth == 0 => {
                        found_colon = true;
                        break;
                    }
                    _ => {}
                }
            }
            header_buf.push_str(l);
            header_buf.push(' ');
            if found_colon {
                break;
            }
            header_end += 1;
        }
        if !found_colon {
            continue;
        }

        // Parse `def __init__(self, name: T, age: int) -> ...:`
        let header = header_buf.trim();
        let after_def = match header
            .strip_prefix(|c: char| c.is_whitespace())
            .unwrap_or(header)
            .strip_prefix("def __init__")
        {
            Some(s) => s.trim(),
            None => continue,
        };
        let paren_open = match after_def.find('(') {
            Some(i) => i,
            None => continue,
        };
        // Find the matching `)` at depth 0.
        let bytes = after_def.as_bytes();
        let mut d: i32 = 0;
        let mut paren_close: Option<usize> = None;
        for (i, b) in bytes.iter().enumerate().skip(paren_open) {
            match *b {
                b'(' | b'[' | b'{' => d += 1,
                b')' | b']' | b'}' => {
                    d -= 1;
                    if d == 0 && *b == b')' {
                        paren_close = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(paren_close) = paren_close else {
            continue;
        };
        let params_src = &after_def[paren_open + 1..paren_close];
        let params: Vec<&str> = split_top_level_commas(params_src)
            .into_iter()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        // Drop `self`; everything else must have a type annotation and
        // no default. Bail if `*args` / `**kwargs` appears anywhere.
        if params.first().copied() != Some("self") {
            continue;
        }
        let mut typed_params: Vec<(String, String)> = Vec::new();
        let mut malformed = false;
        for p in &params[1..] {
            if p.starts_with('*') || p.contains('=') {
                malformed = true;
                break;
            }
            let Some(colon) = p.find(':') else {
                malformed = true;
                break;
            };
            let name = p[..colon].trim().to_owned();
            let ty = p[colon + 1..].trim().to_owned();
            if name.is_empty() || ty.is_empty() {
                malformed = true;
                break;
            }
            typed_params.push((name, ty));
        }
        if malformed || typed_params.is_empty() {
            // Empty-param `__init__(self)` is a no-op constructor; it's
            // safe to drop too, but we already special-case that below
            // by requiring at least one parameter to keep the diagnostic
            // narrow. Falling through is fine — class stays on the
            // `class!` path with an explicit empty init.
            continue;
        }

        // Walk the body. Body extends from header_end+1 up to the next
        // line whose indent is <= class_indent (or EOF).
        let mut body_lines: Vec<usize> = Vec::new();
        let mut i = header_end + 1;
        while i < lines.len() {
            let l = lines[i];
            let lt = l.trim_start();
            if lt.is_empty() || lt.starts_with('#') {
                body_lines.push(i);
                i += 1;
                continue;
            }
            let ind = l.len() - lt.len();
            if ind <= body_indent {
                break;
            }
            body_lines.push(i);
            i += 1;
        }
        // Inside the body, ignore blank/comment lines for the pattern
        // match. Every remaining line must be `self.NAME = NAME` at
        // body_indent + 4 (one level deeper than the def header).
        let mut assignments: Vec<String> = Vec::new();
        let mut trivial_body = true;
        for li in &body_lines {
            let l = lines[*li];
            let lt = l.trim_start();
            if lt.is_empty() || lt.starts_with('#') {
                continue;
            }
            // The body must consist only of `self.NAME = NAME` lines.
            let Some(rhs) = lt.strip_prefix("self.") else {
                trivial_body = false;
                break;
            };
            let Some(eq) = rhs.find('=') else {
                trivial_body = false;
                break;
            };
            let field = rhs[..eq].trim().to_owned();
            let value = rhs[eq + 1..]
                .trim()
                .trim_end_matches(['\r', '\n'])
                .to_owned();
            // Strip a possible trailing comment.
            let value = value.split('#').next().unwrap_or(&value).trim().to_owned();
            if field != value {
                trivial_body = false;
                break;
            }
            assignments.push(field);
        }
        if !trivial_body {
            continue;
        }
        // Every parameter must have a matching assignment in declaration
        // order. Allowing extra fields would silently lose information;
        // allowing fewer would mean some param goes unused.
        let param_names: Vec<&String> = typed_params.iter().map(|(n, _)| n).collect();
        let assigned_names: Vec<&String> = assignments.iter().collect();
        if param_names != assigned_names {
            continue;
        }

        trivial.insert(idx);
        // Drop the entire __init__ method (header + body).
        for li in init_line..=header_end {
            skip.insert(li);
        }
        for li in body_lines {
            skip.insert(li);
        }
        // Inject `name: type` per param into the class body. By default
        // the fields land immediately after the class header so the
        // rewritten output reads like a normal dataclass-style body.
        //
        // Edge case: if the original class body's first statement is a
        // docstring, injecting after the header would push the string
        // literal out of first-statement position and CPython would no
        // longer set `__doc__` for the class. Detect a leading docstring
        // at `body_indent` and anchor the injection after its last line
        // so `__doc__` is preserved.
        let anchor = leading_docstring_end(&lines, idx, body_indent).unwrap_or(idx);
        inject.insert(anchor, typed_params);
    }

    (trivial, skip, inject)
}

/// If the class body's first statement (skipping blank lines and
/// comments) is a single- or triple-quoted string literal at the
/// expected `body_indent`, return its last line index so callers can
/// anchor field injection after it instead of immediately after the
/// class header. Preserves CPython's `__doc__` for the migrated class.
fn leading_docstring_end(
    lines: &[&str],
    class_header_idx: usize,
    body_indent: usize,
) -> Option<usize> {
    let mut i = class_header_idx + 1;
    while i < lines.len() {
        let raw = lines[i];
        let trim = raw.trim_start();
        if trim.is_empty() || trim.starts_with('#') {
            i += 1;
            continue;
        }
        let indent = raw.len() - trim.len();
        if indent != body_indent {
            return None;
        }
        // Triple-quoted first — match either `"""` or `'''`. Walk forward
        // until the closing delimiter (possibly on the same line for a
        // one-line docstring).
        for triple in ["\"\"\"", "'''"] {
            if let Some(after_open) = trim.strip_prefix(triple) {
                if after_open.contains(triple) {
                    return Some(i);
                }
                let mut j = i + 1;
                while j < lines.len() {
                    if lines[j].contains(triple) {
                        return Some(j);
                    }
                    j += 1;
                }
                return None;
            }
        }
        // Single-line `"..."` or `'...'` docstring on one line.
        let bytes = trim.as_bytes();
        if let Some(&q) = bytes.first() {
            if q == b'"' || q == b'\'' {
                if let Some(end) = trim[1..].find(q as char) {
                    let rest = trim[1 + end + 1..].trim_start();
                    if rest.is_empty() || rest.starts_with('#') {
                        return Some(i);
                    }
                }
            }
        }
        return None;
    }
    None
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

/// Find every `@dataclass(frozen=True[, ...])` (and qualified-form)
/// decorator line. The decorator itself is dropped at rewrite time;
/// the immediately-following class header gets a `frozen` suffix
/// (see [`collect_frozen_class_lines`]).
fn collect_frozen_decorator_lines(source: &str) -> HashSet<usize> {
    let mut out: HashSet<usize> = HashSet::new();
    for (idx, raw) in source.lines().enumerate() {
        let trimmed = raw.trim_start();
        // Check the qualified form first because `@dataclass` is a
        // strict prefix of `@dataclasses.dataclass`.
        let args = if let Some(rest) = trimmed.strip_prefix("@dataclasses.dataclass") {
            rest.trim()
        } else if let Some(rest) = trimmed.strip_prefix("@dataclass") {
            rest.trim()
        } else {
            continue;
        };
        // Empty args (`@dataclass`) cannot be frozen.
        if !args.starts_with('(') {
            continue;
        }
        // Find the matching `)`, skipping over string literals so
        // a stray `)` inside a string (e.g. `@dataclass(msg="done)")`)
        // doesn't fool the scanner. FINDINGS — gemini review of
        // PR #105.
        let Some(close) = find_matching_close_paren(args) else {
            continue;
        };
        let inside = &args[1..close];
        // Look for `frozen=True` as a top-level kwarg. Reject the
        // `frozen=False` and bare `True` (positional) cases — both are
        // unusual and we don't want surprises.
        if args_contain_frozen_true(inside) {
            out.insert(idx);
        }
    }
    out
}

/// Find the byte offset of the `)` that closes the leading `(` in `s`.
///
/// `s` must start with `(`. Walks the bytes tracking paren / bracket
/// depth and the inside-a-string state so a `)` inside a single-line
/// quoted string is skipped. Triple-quoted strings and the rare
/// embedded-newline-in-single-quoted-string-via-backslash case are
/// out of scope — the migrator is best-effort and the input here is
/// always a single source line. Returns `None` if no matching close
/// is found within the slice.
fn find_matching_close_paren(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'(') {
        return None;
    }
    let mut depth = 0i32;
    let mut i = 0;
    let mut in_str: Option<u8> = None; // Some(quote_char) when inside a string
    let mut escape_next = false;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_str {
            if escape_next {
                escape_next = false;
            } else if b == b'\\' {
                escape_next = true;
            } else if b == q {
                in_str = None;
            }
        } else {
            match b {
                b'"' | b'\'' => in_str = Some(b),
                b'(' | b'[' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                b']' => depth -= 1,
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// Find the byte offset of the first `#` in `line` that begins a
/// comment — i.e. the first `#` that is outside any string literal.
/// Returns `None` if no comment is present.
///
/// Uses the same single-line string scanner as `find_matching_close_paren`:
/// triple-quoted strings and backslash-continued strings are out of
/// scope, but the input here is always a single source line so neither
/// case applies in practice.
fn find_comment_start(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut in_str: Option<u8> = None;
    let mut escape_next = false;
    for (i, &b) in bytes.iter().enumerate() {
        if let Some(q) = in_str {
            if escape_next {
                escape_next = false;
            } else if b == b'\\' {
                escape_next = true;
            } else if b == q {
                in_str = None;
            }
        } else {
            match b {
                b'"' | b'\'' => in_str = Some(b),
                b'#' => return Some(i),
                _ => {}
            }
        }
    }
    None
}

/// `true` when `args` (the contents between `@dataclass(` and `)`) has
/// a top-level `frozen=True` keyword argument. Walks the comma-
/// separated list at top-level depth so nested parens/brackets stay
/// grouped.
fn args_contain_frozen_true(args: &str) -> bool {
    for piece in split_top_level_commas(args) {
        let p = piece.trim();
        if p == "frozen=True" || p.starts_with("frozen = True") || p.starts_with("frozen=True ") {
            return true;
        }
        // Tolerate whitespace variants like `frozen = True`.
        if let Some(rest) = p.strip_prefix("frozen") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                if rest.trim() == "True" {
                    return true;
                }
            }
        }
    }
    false
}

/// Find every class header line whose `@dataclass(frozen=True[, ...])`
/// decorator (recorded by [`collect_frozen_decorator_lines`]) sits
/// directly above it (possibly with blank lines or other decorators in
/// between, as long as they sit at the same indent).
fn collect_frozen_class_lines(source: &str) -> HashSet<usize> {
    let frozen_decorators = collect_frozen_decorator_lines(source);
    let lines: Vec<&str> = source.lines().collect();
    let mut out: HashSet<usize> = HashSet::new();
    for (idx, raw) in lines.iter().enumerate() {
        let trimmed = raw.trim_start();
        if !trimmed.starts_with("class ") && trimmed != "class" {
            continue;
        }
        let indent_len = raw.len() - trimmed.len();
        let mut probe = idx;
        while probe > 0 {
            probe -= 1;
            let prev = lines[probe];
            let prev_trim = prev.trim_start();
            if prev_trim.is_empty() {
                continue;
            }
            let prev_indent = prev.len() - prev_trim.len();
            if prev_indent != indent_len {
                break;
            }
            if !prev_trim.starts_with('@') {
                break;
            }
            if frozen_decorators.contains(&probe) {
                out.insert(idx);
                break;
            }
        }
    }
    out
}

/// Append a trailing `frozen` modifier to a class header.
///
/// `class Vec:` → `class Vec frozen:`
/// `class Vec(Base):` → `class Vec(Base) frozen:`
/// `class! Counter:` → left alone (unsupported combination)
fn append_frozen_modifier(header: &str) -> String {
    if !header.starts_with("class ") {
        return header.to_owned();
    }
    let Some(colon_idx) = header.rfind(':') else {
        return header.to_owned();
    };
    let before = &header[..colon_idx];
    let after = &header[colon_idx..];
    format!("{before} frozen{after}")
}

/// Rewrite `class X(Protocol):` to `interface X:`.
///
/// Only single-base `Protocol` is recognised — combined bases need
/// human judgement and are left untouched. The trailing colon /
/// comment / continuation is preserved verbatim. `Protocol[T]`
/// (generic Protocol) is also recognised and the type parameters are
/// kept: `class X(Protocol[T]):` → `interface X[T]:`.
fn rewrite_protocol_class(line: &str) -> Option<String> {
    let rest = line.strip_prefix("class ")?;
    let open = rest.find('(')?;
    let name = rest[..open].trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    // Find matching `)` at depth 0.
    let bytes = rest.as_bytes();
    let mut depth = 0i32;
    let mut close: Option<usize> = None;
    for (i, b) in bytes.iter().enumerate().skip(open) {
        match *b {
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
    let inside = rest[open + 1..close].trim();
    let trailer = &rest[close + 1..];

    let bases: Vec<&str> = split_top_level_commas(inside)
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if bases.len() != 1 {
        return None;
    }
    let base = bases[0];
    // Accept `Protocol`, `typing.Protocol`, `Protocol[T, U]`,
    // `typing.Protocol[T, U]`. The `[...]` suffix becomes the type
    // params on the `interface` form.
    let stripped = base
        .strip_prefix("typing.Protocol")
        .or_else(|| base.strip_prefix("Protocol"))?;
    let type_params: Option<&str> = if stripped.is_empty() {
        None
    } else if let Some(inner) = stripped.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        Some(inner.trim())
    } else {
        // Suffix that isn't `[...]` — not a Protocol base.
        return None;
    };
    let head = match type_params {
        Some(params) => format!("interface {}[{}]", name, params),
        None => format!("interface {}", name),
    };
    Some(format!("{head}{trailer}"))
}

/// Rewrite `NAME = NewType("NAME", BASE)` to `newtype NAME = BASE`.
///
/// Returns `None` when the line isn't a NewType binding, when the LHS
/// name doesn't match the string literal, or when the second argument
/// isn't a parseable type expression.
fn rewrite_newtype_declaration(line: &str) -> Option<String> {
    // Strip a trailing comment, but only at a `#` that's outside any
    // string literal — `UserId = NewType("X#Y", int)` is legal and
    // must round-trip. FINDINGS — gemini review of PR #105.
    let comment_start = find_comment_start(line);
    let code = match comment_start {
        Some(i) => line[..i].trim(),
        None => line.trim(),
    };
    let eq = code.find('=')?;
    let lhs = code[..eq].trim();
    if lhs.is_empty()
        || !lhs.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        || lhs.starts_with(|c: char| c.is_ascii_digit())
    {
        return None;
    }
    let rhs = code[eq + 1..].trim();
    let inner = rhs
        .strip_prefix("NewType(")
        .or_else(|| rhs.strip_prefix("typing.NewType("))?
        .strip_suffix(')')?;
    let args: Vec<&str> = split_top_level_commas(inner)
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if args.len() != 2 {
        return None;
    }
    // Accept `"NAME"` or `'NAME'`.
    let name_arg = args[0];
    let name_in_str = name_arg
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| {
            name_arg
                .strip_prefix('\'')
                .and_then(|s| s.strip_suffix('\''))
        })?;
    if name_in_str != lhs {
        return None;
    }
    let base = args[1];
    if base.is_empty() {
        return None;
    }
    Some(format!("newtype {lhs} = {base}"))
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
    fn trivial_init_class_loses_init_and_stays_plain_class() {
        // FINDINGS #33: `class Box(Generic[T]): def __init__(self, value: T) -> None: self.value = value`
        // is a textbook trivial init — strip it so Typhon's
        // auto-`__init__` synthesis takes over, and keep the header
        // as plain `class`, not `class!`.
        let src = "\
from typing import Generic, TypeVar
T = TypeVar(\"T\")

class Box(Generic[T]):
    def __init__(self, value: T) -> None:
        self.value = value
";
        let out = migrate_source(src);
        // No `class!` and no `__init__` left over.
        assert!(
            !out.contains("class!"),
            "trivial init should NOT promote to class!, got:\n{out}"
        );
        assert!(
            !out.contains("def __init__"),
            "trivial __init__ should be stripped, got:\n{out}"
        );
        // Field declaration injected from the parameter type.
        assert!(
            out.contains("value: T"),
            "must inject `value: T` field, got:\n{out}"
        );
        // PEP 695 generic rewrite still fires.
        assert!(
            out.contains("class Box[T]:"),
            "header should be `class Box[T]:`, got:\n{out}"
        );
    }

    #[test]
    fn nontrivial_init_still_becomes_class_bang() {
        // The dropped-init shortcut must NOT fire when the body does
        // anything beyond `self.field = field` — `super().__init__()`,
        // computed values, side effects must keep their hand-rolled
        // constructor (and hence `class!`).
        let src = "\
class MyModel(nn.Module):
    def __init__(self, layers: int) -> None:
        super().__init__()
        self.layers = layers
";
        let out = migrate_source(src);
        assert!(
            out.contains("class! MyModel(nn.Module):"),
            "non-trivial init must stay on class!, got:\n{out}"
        );
        assert!(
            out.contains("def __init__"),
            "init body must survive, got:\n{out}"
        );
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

    // ── Rule 7: frozen dataclass → `class X frozen:` ────────────────────────

    #[test]
    fn dataclass_frozen_true_becomes_frozen_class() {
        let src = "\
from dataclasses import dataclass

@dataclass(frozen=True)
class Vec:
    x: int
    y: int
";
        let out = migrate_source(src);
        assert!(
            out.contains("class Vec frozen:"),
            "expected frozen modifier, got:\n{out}"
        );
        assert!(
            !out.contains("@dataclass(frozen=True)"),
            "decorator must be dropped, got:\n{out}"
        );
    }

    #[test]
    fn dataclass_qualified_frozen_true_becomes_frozen_class() {
        let src = "\
import dataclasses

@dataclasses.dataclass(frozen=True, slots=True)
class Vec:
    x: int
";
        let out = migrate_source(src);
        assert!(out.contains("class Vec frozen:"), "got:\n{out}");
        assert!(
            !out.contains("@dataclasses.dataclass"),
            "decorator must be dropped, got:\n{out}"
        );
    }

    #[test]
    fn dataclass_without_frozen_kw_unchanged_class_header() {
        let src = "\
@dataclass(slots=True)
class Point:
    x: int
";
        let out = migrate_source(src);
        assert!(!out.contains("frozen"), "must not add frozen, got:\n{out}");
    }

    #[test]
    fn dataclass_frozen_with_base_keeps_base() {
        let src = "\
@dataclass(frozen=True)
class Vec(Base):
    x: int
";
        let out = migrate_source(src);
        assert!(out.contains("class Vec(Base) frozen:"), "got:\n{out}");
    }

    // ── Rule 8: Protocol → interface ────────────────────────────────────────

    #[test]
    fn protocol_base_becomes_interface() {
        let src = "\
from typing import Protocol

class Drawable(Protocol):
    def draw(self) -> None: ...
";
        let out = migrate_source(src);
        assert!(
            out.contains("interface Drawable:"),
            "expected interface decl, got:\n{out}"
        );
        assert!(
            !out.contains("from typing import Protocol"),
            "Protocol import must be dropped, got:\n{out}"
        );
    }

    #[test]
    fn generic_protocol_becomes_interface_with_params() {
        let src = "\
from typing import Protocol, TypeVar
T = TypeVar(\"T\")
class Box(Protocol[T]):
    def get(self) -> T: ...
";
        let out = migrate_source(src);
        assert!(
            out.contains("interface Box[T]:"),
            "expected `interface Box[T]:`, got:\n{out}"
        );
    }

    #[test]
    fn protocol_with_extra_base_left_alone() {
        // Combined bases need human judgement.
        let src = "\
from typing import Protocol

class Drawable(Protocol, ABC):
    def draw(self) -> None: ...
";
        let out = migrate_source(src);
        assert!(
            !out.contains("interface Drawable"),
            "multi-base Protocol must be left alone, got:\n{out}"
        );
    }

    // ── Rule 9: NewType → newtype ───────────────────────────────────────────

    #[test]
    fn newtype_declaration_rewrites_to_newtype_keyword() {
        let src = "\
from typing import NewType

UserId = NewType(\"UserId\", int)
Email = NewType(\"Email\", str)
";
        let out = migrate_source(src);
        assert!(
            out.contains("newtype UserId = int"),
            "expected newtype UserId, got:\n{out}"
        );
        assert!(
            out.contains("newtype Email = str"),
            "expected newtype Email, got:\n{out}"
        );
        assert!(
            !out.contains("from typing import NewType"),
            "NewType import must be dropped, got:\n{out}"
        );
    }

    #[test]
    fn newtype_with_mismatched_name_left_alone() {
        // `UserId = NewType("user_id", int)` — the string doesn't match
        // the LHS, so the rewrite skips it (likely a copy-paste bug).
        let src = "UserId = NewType(\"user_id\", int)\n";
        let out = migrate_source(src);
        assert!(
            !out.contains("newtype"),
            "name mismatch must skip rewrite, got:\n{out}"
        );
    }

    // ── string-aware scanners (gemini PR #105 review) ───────────────────────

    #[test]
    fn frozen_decorator_paren_scan_skips_paren_inside_string() {
        // `@dataclass(metadata="done)")` — the closing paren inside
        // the string must NOT be mistaken for the decorator's
        // closing `)`, which would truncate the decorator scan and
        // miss the `frozen=True` kwarg later on the same line.
        let src = "\
@dataclass(metadata=\"done)\", frozen=True)
class Vec:
    x: int
";
        let out = migrate_source(src);
        assert!(
            out.contains("class Vec frozen:"),
            "string-embedded `)` must not break the paren scan; got:\n{out}"
        );
    }

    #[test]
    fn newtype_hash_inside_string_not_treated_as_comment() {
        // `UserId = NewType("User#ID", int)` — the `#` is inside the
        // string literal and must not be stripped as a comment.
        let src = "UserId = NewType(\"User#ID\", int)\n";
        let out = migrate_source(src);
        // String contents differ from LHS name (`User#ID` ≠ `UserId`)
        // so the rewrite still skips — that's the correct behaviour.
        // The important assertion is that the scan didn't panic / mis-
        // strip; verify the original line survives intact.
        assert!(
            out.contains("NewType(\"User#ID\", int)"),
            "string-embedded `#` must not be stripped; got:\n{out}"
        );
    }

    #[test]
    fn newtype_hash_inside_string_matches_lhs_rewrites() {
        // Same name on both sides — the `#`-inside-string handling
        // means the rewrite proceeds even with an unusual literal.
        let src = "User_ID = NewType(\"User_ID\", int)  # the canonical id\n";
        let out = migrate_source(src);
        assert!(
            out.contains("newtype User_ID = int"),
            "rewrite must work with a trailing real comment; got:\n{out}"
        );
    }
}
