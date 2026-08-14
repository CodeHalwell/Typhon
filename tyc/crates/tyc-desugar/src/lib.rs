//! Typhon AST → Python AST lowering (Phase 2+).
//!
//! Phase 2 implements class desugaring: plain Typhon `class` definitions are
//! rewritten to `@dataclasses.dataclass(slots=True)` Python classes, and the
//! required `import dataclasses` statement is injected when needed.
//!
//! Using the qualified `dataclasses.dataclass` form avoids name-shadowing
//! hygiene issues that arise with a bare `dataclass` name imported into the
//! module namespace.
//!
//! The import is inserted after any leading module docstring and
//! `from __future__ import` statements so the emitted file is always valid
//! Python.
//!
//! The transformation is applied recursively across the full statement tree, so
//! classes defined inside functions or other classes are also desugared.
//!
//! Future phases will add desugaring for sealed unions, the `?` operator,
//! `with`-chains, and other Typhon-specific constructs.

use std::collections::{HashMap, HashSet};

use ruff_python_ast::name::Name;
use ruff_python_ast::{
    self as ast, Alias, Arguments, AtomicNodeIndex, Decorator, ExceptHandler, Expr, ExprAttribute,
    ExprBooleanLiteral, ExprCall, ExprContext, ExprName, ExprStringLiteral, Identifier, Keyword,
    ModModule, Operator, Parameter, ParameterWithDefault, Parameters, Pattern, Stmt, StmtAssign,
    StmtImport, StmtImportFrom, StringLiteral, StringLiteralFlags, StringLiteralValue,
};
use ruff_text_size::TextRange;

// ── public API ───────────────────────────────────────────────────────────────

/// Output of the module desugaring pass.
pub struct DesugarOutput {
    /// The desugared Python-compatible AST.
    pub module: ModModule,
    /// Whether the emitted module will import from `typhon_runtime`. When
    /// true, the build command must write `typhon_runtime.py` alongside the
    /// other output files so the generated import can resolve at runtime.
    pub needs_typhon_runtime: bool,
}

/// Options that customise the desugar pass for a single module.
#[derive(Debug, Clone)]
pub struct DesugarOptions {
    /// Names of top-level functions to wrap in `@functools.cache`. Populated
    /// from the purity analyser when the user opts into `@memo` /
    /// `@pure(memo=True)` / `[strictness] auto-memoise = true`.
    pub memoise_functions: Vec<String>,
    /// Byte offsets (start of the line) at which a `class!` declaration
    /// appears in the *preprocessed* source.  A class whose `TextRange`
    /// starts at or just after one of these offsets is treated as raw and
    /// the `@dataclass` decorator injection is skipped.  Populated from
    /// the preprocessor's `raw_class_lines` via `line_byte_starts`.
    pub raw_class_line_starts: Vec<u32>,
    /// Byte offsets (start of the line) at which a `class NAME frozen:`
    /// declaration appears in the *preprocessed* source.  A class whose
    /// `TextRange` starts at or just after one of these offsets gets
    /// `@dataclasses.dataclass(slots=True, frozen=True)` instead of the
    /// default decorator.  Populated from the preprocessor's
    /// `frozen_class_lines` via `line_byte_starts`.
    pub frozen_class_line_starts: Vec<u32>,
    /// Byte offsets (start of the line) at which a `plain class NAME:`
    /// declaration appears in the *preprocessed* source.  A class whose
    /// `TextRange` starts at or just after one of these offsets skips the
    /// `@dataclass` decorator entirely AND skips the raw-class `__init__`
    /// synthesis — the class body is emitted exactly as written.
    /// Populated from the preprocessor's `plain_class_lines` via
    /// `line_byte_starts`.
    pub plain_class_line_starts: Vec<u32>,
    /// User-supplied class base names whose subclasses should NOT receive
    /// the auto-`@dataclass` decoration.  Matched by *last* identifier
    /// segment, so an entry of `"App"` matches both `class T(App):` and
    /// `class T(textual.App):`.  Plumbed in from `typhon.toml`
    /// (`[emit] skip-decoration-bases`).
    pub skip_decoration_bases: Vec<String>,
    /// Names declared with the `pub` modifier in this module, in source
    /// order. When non-empty, the desugar pass injects an
    /// `__all__ = [...]` list at the top of the emitted module so
    /// `from foo import *` brings in exactly the marked surface and
    /// downstream tooling (Sphinx autoapi, pyright re-export tracking,
    /// IDE auto-import filters) sees the public API. An empty
    /// `pub_names` is intentionally a no-op so legacy `.ty` files
    /// that pre-date the `pub` keyword keep their current behaviour.
    pub pub_names: Vec<String>,
    /// Value for the `extra` keyword argument in the auto-injected
    /// `model_config = ConfigDict(extra=…)` statement for Pydantic
    /// `model` classes.  Defaults to `"forbid"` (reject unexpected
    /// fields).  Accepted values: `"forbid"`, `"ignore"`, `"allow"`.
    /// Plumbed from `[emit] model-extra` in `typhon.toml`.
    pub model_extra: String,
    /// When `true`, inject a `typhon_runtime.traceback.install()` call at the
    /// top of this module's `if __name__ == "__main__":` block so an uncaught
    /// exception's traceback is rewritten to point at `.ty` source (via the
    /// emitted `.py.map` sidecars) with no manual `tyc trace` step. Plumbed
    /// from `[emit] traceback-remap` in `typhon.toml`; default `false` so
    /// existing projects (and runtime-free entry points) are unaffected.
    pub traceback_remap: bool,
}

/// Desugar a Typhon module AST into a plain Python AST.
///
/// Performs three transformations:
///
/// 1. **Dataclass desugaring** — every `class` definition that does not already
///    carry a `@dataclass` / `@dataclasses.dataclass` decorator *and* does not
///    inherit from `BaseModel` gets `@dataclasses.dataclass(slots=True)`
///    prepended, and `import dataclasses` is injected when needed. Recursive.
///
/// 2. **Pydantic model desugaring** — every `class` that *does* inherit from
///    `BaseModel` (produced by the `model` keyword preprocessor) is left
///    without the dataclass decorator, and `from pydantic import BaseModel` is
///    injected after any leading docstring / future-imports.
///
/// 3. **Result import injection** — if the module references `Ok`, `Err`, or
///    `Result` anywhere, `from typhon_runtime import Ok, Err, Result` is
///    injected after any leading docstring and future-imports so the generated
///    Python can use those names.
impl Default for DesugarOptions {
    fn default() -> Self {
        Self {
            memoise_functions: Vec::new(),
            raw_class_line_starts: Vec::new(),
            frozen_class_line_starts: Vec::new(),
            plain_class_line_starts: Vec::new(),
            skip_decoration_bases: Vec::new(),
            pub_names: Vec::new(),
            model_extra: "forbid".into(),
            traceback_remap: false,
        }
    }
}

pub fn desugar_module(module: &ModModule) -> DesugarOutput {
    desugar_module_with(module, DesugarOptions::default())
}

/// Same as [`desugar_module`] but accepts caller-supplied [`DesugarOptions`].
///
/// Used by `tyc build` to thread purity-analysis results (which top-level
/// functions opted into `@memo` and therefore need an injected
/// `@functools.cache` decorator) through to the desugar pass.
pub fn desugar_module_with(module: &ModModule, options: DesugarOptions) -> DesugarOutput {
    let mut desugared_mod = desugar_mod_module_with(module, &options);

    // `[emit] traceback-remap`: prepend `typhon_runtime.traceback.install()`
    // to the entry module's `if __name__ == "__main__":` block so uncaught
    // exceptions are reported against `.ty` source automatically. Done before
    // the runtime-need detection below so the injected import flags the
    // typhon_runtime package for emission.
    let injected_traceback =
        options.traceback_remap && inject_traceback_install(&mut desugared_mod.body);

    let has_result_usage = stmts_use_result_names(&module.body);
    let has_any_runtime_import = has_any_typhon_runtime_import(&module.body);
    let import_covers_all = typhon_runtime_import_covers_all(&module.body);
    // Detect `typhon_runtime.tasks.spawn(...)` (lowered `go`) and
    // `typhon_runtime.lazy.lazy_import(...)` / `typhon_runtime.lazy.lazy_val(...)`
    // (lowered `lazy import` / `lazy val`) calls so we know to import
    // the runtime as a module and emit the helper file.
    let has_runtime_qualified = expr_tree_uses_runtime_attribute(&desugared_mod.body);
    // The build must write `typhon_runtime.py` whenever the emitted
    // module will reference it — either because we detected an Ok/
    // Err/Result name, because the user explicitly imported it, or
    // because a `go`/`lazy` lowering produced a qualified reference.
    // Freeze references will need the runtime helper too — set the
    // flag here so `tyc build` emits the typhon_runtime/ package even
    // when no other runtime feature is used.
    let needs_freeze_runtime = stmts_use_freeze_call(&desugared_mod.body);
    // `try_result(...)` is the exception→Result bridging combinator; its use
    // pulls in `from typhon_runtime import try_result` (and the runtime
    // package) independently of the `Ok`/`Err`/`Result` family import.
    let needs_try_result = stmts_use_try_result(&desugared_mod.body);
    let needs_typhon_runtime = has_result_usage
        || has_any_runtime_import
        || has_runtime_qualified
        || needs_freeze_runtime
        || needs_try_result
        || injected_traceback;
    // Only skip injection when an existing `from typhon_runtime
    // import …` already covers all three names. A partial import
    // (e.g. just `Ok`) would leave `Err`/`Result` undefined, so we
    // still inject. A bare `import typhon_runtime` doesn't bring
    // `Ok`/`Err`/`Result` into scope, so we also still inject.
    let inject_result_import = has_result_usage && !import_covers_all;

    // Inject pydantic imports for any class that inherits from BaseModel
    // (produced by the `model` keyword preprocessor).  Track BaseModel and
    // ConfigDict independently: a module that already has
    // `from pydantic import BaseModel` still needs `ConfigDict` imported
    // because the desugarer injects `model_config = ConfigDict(extra="forbid")`.
    let needs_pydantic = stmts_use_basemodel(&desugared_mod.body);
    let inject_basemodel = needs_pydantic && !has_pydantic_import(&desugared_mod.body);
    let inject_config_dict = needs_pydantic && !has_config_dict_import(&desugared_mod.body);

    // `interface Name:` lowers to `class Name(Protocol):` — ensure
    // `from typing import Protocol` is in scope.
    let needs_protocol = stmts_use_protocol_base(&desugared_mod.body);
    let inject_protocol = needs_protocol && !has_protocol_import(&desugared_mod.body);

    // The checker's prelude accepts `Iterator` / `Sequence` / `Callable` /
    // … in annotations without an import, but the emitted Python only
    // survived because of `from __future__ import annotations` — runtime
    // annotation resolution (FastAPI DI, `typing.get_type_hints`,
    // dataclass introspection) would NameError. Inject the
    // `collections.abc` import for any such name that isn't already
    // bound at module level.
    let abc_names_to_import = collect_unimported_abc_annotation_names(&desugared_mod.body);

    // `newtype Name = Base` lowers to `Name = NewType("Name", Base)` —
    // ensure `from typing import NewType` is in scope.
    let needs_newtype = stmts_use_newtype_call(&desugared_mod.body);
    let inject_newtype = needs_newtype && !has_newtype_import(&desugared_mod.body);

    // `freeze let X = expr` lowers to `let X = __typhon_freeze__(expr)`
    // — ensure `from typhon_runtime.freeze import deep_freeze as
    // __typhon_freeze__` is in scope and the typhon_runtime package
    // gets emitted alongside the output.
    let needs_freeze = stmts_use_freeze_call(&desugared_mod.body);
    let inject_freeze = needs_freeze && !has_freeze_import(&desugared_mod.body);

    // `from typhon_runtime import try_result` — injected only when the
    // combinator is used and not already imported.
    let inject_try_result = needs_try_result && !has_try_result_import(&desugared_mod.body);

    // `gather:` lowers to `asyncio.TaskGroup` and best-effort to
    // `asyncio.gather(...)` — ensure `import asyncio` is in scope.
    let needs_asyncio = stmts_use_asyncio_qualified(&desugared_mod.body);
    let inject_asyncio = needs_asyncio && !has_asyncio_import(&desugared_mod.body);

    // `enum Name:` lowers to `class Name(enum.Enum):` with members assigned
    // `enum.auto()` — ensure `import enum` is in scope.
    let needs_enum = stmts_use_enum_base(&desugared_mod.body);
    let inject_enum = needs_enum && !has_enum_import(&desugared_mod.body);

    // `go` and `lazy` lower to `typhon_runtime.tasks.spawn(...)` /
    // `typhon_runtime.lazy.…` — ensure a bare `import typhon_runtime`
    // is in scope when the user hasn't already arranged one.
    let inject_runtime_module =
        has_runtime_qualified && !has_bare_typhon_runtime_import(&desugared_mod.body);

    let mut body = desugared_mod.body;
    let insert_at = import_insert_pos(&body);

    // `pub` synthesis: when the source declared at least one `pub`
    // name, emit `__all__ = ["a", "b", …]` right after the import
    // block so re-exporters and `import *` consumers see the public
    // surface. Skipped if the module already provides its own
    // `__all__` so handwritten lists win. Compute the insertion
    // need now — but defer the actual insert until AFTER every
    // injected import has landed (otherwise repeated `Vec::insert`
    // at the same index would splice `__all__` into the middle of
    // the import block).
    let inject_dunder_all =
        !options.pub_names.is_empty() && !body.iter().any(is_dunder_all_assignment);

    // Insert imports in reverse order so later `insert_at` calls don't
    // shift indices of earlier insertions. The relative order ends up
    // pydantic → result → freeze → newtype → protocol → asyncio →
    // runtime_module at the top of the file.
    let mut imports_inserted: usize = 0;
    let mut inject = |body: &mut Vec<Stmt>, stmt: Stmt| {
        body.insert(insert_at, stmt);
        imports_inserted += 1;
    };
    if inject_runtime_module {
        inject(&mut body, make_bare_typhon_runtime_import());
    }
    if inject_enum {
        inject(&mut body, make_enum_import());
    }
    if inject_asyncio {
        inject(&mut body, make_asyncio_import());
    }
    if inject_protocol {
        inject(&mut body, make_protocol_import());
    }
    if !abc_names_to_import.is_empty() {
        inject(&mut body, make_collections_abc_import(&abc_names_to_import));
    }
    if inject_newtype {
        inject(&mut body, make_newtype_import());
    }
    if inject_freeze {
        inject(&mut body, make_freeze_import());
    }
    if inject_result_import {
        inject(&mut body, make_typhon_runtime_import());
    }
    if inject_try_result {
        inject(&mut body, make_try_result_import());
    }
    // Emit the fewest imports possible: combine into one statement when
    // both are needed, otherwise emit just the missing one.
    if inject_basemodel && inject_config_dict {
        inject(&mut body, make_pydantic_basemodel_import()); // includes both
    } else if inject_basemodel {
        inject(&mut body, make_pydantic_basemodel_only_import());
    } else if inject_config_dict {
        inject(&mut body, make_config_dict_only_import());
    }
    // `inject` borrows `imports_inserted`; the closure's last call
    // sits above, so its borrow ends here naturally and `__all__`
    // can read `imports_inserted` again.
    let _ = inject;

    if inject_dunder_all {
        // Place `__all__` AFTER the last injected import so it never
        // splits the import block. `insert_at + imports_inserted` is
        // the index right after the run of imports that landed above.
        body.insert(
            insert_at + imports_inserted,
            make_dunder_all(&options.pub_names),
        );
    }

    DesugarOutput {
        module: ModModule {
            range: desugared_mod.range,
            node_index: AtomicNodeIndex::NONE,
            body,
        },
        needs_typhon_runtime,
    }
}

// ── Runtime-name detection ─────────────────────────────────────────────────────

/// Return `true` if any statement in `stmts` (or its nested bodies) references
/// the identifiers `Ok`, `Err`, or `Result` — gates injection of
/// `from typhon_runtime import Ok, Err, Result`.
fn stmts_use_result_names(stmts: &[Stmt]) -> bool {
    stmts_reference_names(stmts, &["Ok", "Err", "Result"])
}

/// Return `true` if any statement references `try_result`, the exception→
/// `Result` bridging combinator. Gates an independent
/// `from typhon_runtime import try_result` injection so a module that only
/// uses `try_result` (never a bare `Ok`/`Err`) still gets the import.
fn stmts_use_try_result(stmts: &[Stmt]) -> bool {
    stmts_reference_names(stmts, &["try_result"])
}

/// Generic structural walk: `true` when any statement (or a nested body)
/// references one of the bare identifiers in `names`. Shared by the
/// `Ok`/`Err`/`Result` and `try_result` runtime-import injectors so both
/// traverse the full statement/expression grammar identically.
fn stmts_reference_names(stmts: &[Stmt], names: &[&str]) -> bool {
    stmts.iter().any(|s| stmt_references_names(s, names))
}

fn stmt_references_names(stmt: &Stmt, names: &[&str]) -> bool {
    match stmt {
        Stmt::FunctionDef(f) => {
            f.returns
                .as_ref()
                .is_some_and(|r| expr_references_names(r, names))
                || parameters_reference_names(&f.parameters, names)
                || f.decorator_list
                    .iter()
                    .any(|d| expr_references_names(&d.expression, names))
                || stmts_reference_names(&f.body, names)
        }
        Stmt::ClassDef(c) => {
            c.decorator_list
                .iter()
                .any(|d| expr_references_names(&d.expression, names))
                || c.bases().iter().any(|b| expr_references_names(b, names))
                || c.keywords()
                    .iter()
                    .any(|k| expr_references_names(&k.value, names))
                || stmts_reference_names(&c.body, names)
        }
        Stmt::AnnAssign(a) => {
            expr_references_names(&a.annotation, names)
                || a.value
                    .as_ref()
                    .is_some_and(|v| expr_references_names(v, names))
                || expr_references_names(&a.target, names)
        }
        Stmt::Assign(a) => {
            expr_references_names(&a.value, names)
                || a.targets.iter().any(|t| expr_references_names(t, names))
        }
        Stmt::AugAssign(a) => {
            expr_references_names(&a.target, names) || expr_references_names(&a.value, names)
        }
        Stmt::Return(r) => r
            .value
            .as_ref()
            .is_some_and(|v| expr_references_names(v, names)),
        Stmt::Expr(e) => expr_references_names(&e.value, names),
        Stmt::If(i) => {
            expr_references_names(&i.test, names)
                || stmts_reference_names(&i.body, names)
                || i.elif_else_clauses.iter().any(|c| {
                    c.test
                        .as_ref()
                        .is_some_and(|t| expr_references_names(t, names))
                        || stmts_reference_names(&c.body, names)
                })
        }
        Stmt::While(w) => {
            expr_references_names(&w.test, names)
                || stmts_reference_names(&w.body, names)
                || stmts_reference_names(&w.orelse, names)
        }
        Stmt::For(f) => {
            expr_references_names(&f.target, names)
                || expr_references_names(&f.iter, names)
                || stmts_reference_names(&f.body, names)
                || stmts_reference_names(&f.orelse, names)
        }
        Stmt::With(w) => {
            w.items.iter().any(|item| {
                expr_references_names(&item.context_expr, names)
                    || item
                        .optional_vars
                        .as_ref()
                        .is_some_and(|v| expr_references_names(v, names))
            }) || stmts_reference_names(&w.body, names)
        }
        Stmt::Try(t) => {
            stmts_reference_names(&t.body, names)
                || t.handlers.iter().any(|handler| {
                    let ExceptHandler::ExceptHandler(h) = handler;
                    h.type_
                        .as_ref()
                        .is_some_and(|ty| expr_references_names(ty, names))
                        || stmts_reference_names(&h.body, names)
                })
                || stmts_reference_names(&t.orelse, names)
                || stmts_reference_names(&t.finalbody, names)
        }
        Stmt::Match(m) => {
            expr_references_names(&m.subject, names)
                || m.cases.iter().any(|case| {
                    case.guard
                        .as_ref()
                        .is_some_and(|g| expr_references_names(g, names))
                        || pattern_references_names(&case.pattern, names)
                        || stmts_reference_names(&case.body, names)
                })
        }
        Stmt::Raise(r) => {
            r.exc
                .as_ref()
                .is_some_and(|e| expr_references_names(e, names))
                || r.cause
                    .as_ref()
                    .is_some_and(|c| expr_references_names(c, names))
        }
        Stmt::Assert(a) => {
            expr_references_names(&a.test, names)
                || a.msg
                    .as_ref()
                    .is_some_and(|m| expr_references_names(m, names))
        }
        Stmt::Delete(d) => d.targets.iter().any(|t| expr_references_names(t, names)),
        Stmt::TypeAlias(t) => {
            expr_references_names(&t.name, names) || expr_references_names(&t.value, names)
        }
        _ => false,
    }
}

fn parameters_reference_names(params: &Parameters, names: &[&str]) -> bool {
    let plain_param = |p: &Parameter| {
        p.annotation
            .as_ref()
            .is_some_and(|a| expr_references_names(a, names))
    };
    let with_default = |p: &ParameterWithDefault| {
        plain_param(&p.parameter)
            || p.default
                .as_ref()
                .is_some_and(|d| expr_references_names(d, names))
    };
    params.posonlyargs.iter().any(with_default)
        || params.args.iter().any(with_default)
        || params.kwonlyargs.iter().any(with_default)
        || params.vararg.as_ref().is_some_and(|a| plain_param(a))
        || params.kwarg.as_ref().is_some_and(|a| plain_param(a))
}

/// Walks every sub-pattern in `pattern`, returning `true` when any
/// `case Ok(...)` / `case Err(...)` / `case Result(...)` reference is
/// found.  Without this, a file that only ever matches on a `Result`
/// (never constructs one or returns one) would skip the auto-import
/// injection and the emitted Python would `NameError` at runtime.
fn pattern_references_names(pattern: &Pattern, names: &[&str]) -> bool {
    match pattern {
        Pattern::MatchValue(ast::PatternMatchValue { value, .. }) => {
            expr_references_names(value, names)
        }
        Pattern::MatchSingleton(_) => false,
        Pattern::MatchSequence(ast::PatternMatchSequence { patterns, .. }) => {
            patterns.iter().any(|p| pattern_references_names(p, names))
        }
        Pattern::MatchMapping(ast::PatternMatchMapping { keys, patterns, .. }) => {
            keys.iter().any(|k| expr_references_names(k, names))
                || patterns.iter().any(|p| pattern_references_names(p, names))
        }
        Pattern::MatchClass(ast::PatternMatchClass { cls, arguments, .. }) => {
            expr_references_names(cls, names)
                || arguments
                    .patterns
                    .iter()
                    .any(|p| pattern_references_names(p, names))
                || arguments
                    .keywords
                    .iter()
                    .any(|kw| pattern_references_names(&kw.pattern, names))
        }
        Pattern::MatchStar(_) => false,
        Pattern::MatchAs(ast::PatternMatchAs { pattern, .. }) => pattern
            .as_ref()
            .is_some_and(|p| pattern_references_names(p, names)),
        Pattern::MatchOr(ast::PatternMatchOr { patterns, .. }) => {
            patterns.iter().any(|p| pattern_references_names(p, names))
        }
    }
}

fn expr_references_names(expr: &Expr, names: &[&str]) -> bool {
    match expr {
        Expr::Name(n) => names.contains(&n.id.as_str()),
        Expr::Call(c) => {
            expr_references_names(&c.func, names)
                || c.arguments
                    .args
                    .iter()
                    .any(|a| expr_references_names(a, names))
                || c.arguments
                    .keywords
                    .iter()
                    .any(|k| expr_references_names(&k.value, names))
        }
        Expr::Subscript(s) => {
            expr_references_names(&s.value, names) || expr_references_names(&s.slice, names)
        }
        Expr::BinOp(b) => {
            expr_references_names(&b.left, names) || expr_references_names(&b.right, names)
        }
        Expr::BoolOp(b) => b.values.iter().any(|v| expr_references_names(v, names)),
        Expr::UnaryOp(u) => expr_references_names(&u.operand, names),
        Expr::Named(n) => {
            expr_references_names(&n.target, names) || expr_references_names(&n.value, names)
        }
        Expr::Compare(c) => {
            expr_references_names(&c.left, names)
                || c.comparators
                    .iter()
                    .any(|cc| expr_references_names(cc, names))
        }
        Expr::Lambda(l) => {
            // Also inspect parameter defaults (`lambda x=Ok(1): x`) — a name
            // referenced only there must still trigger the runtime-import
            // injection, or the emitted Python would `NameError` at runtime.
            l.parameters
                .as_ref()
                .is_some_and(|p| parameters_reference_names(p, names))
                || expr_references_names(&l.body, names)
        }
        Expr::If(i) => {
            expr_references_names(&i.test, names)
                || expr_references_names(&i.body, names)
                || expr_references_names(&i.orelse, names)
        }
        Expr::Tuple(t) => t.elts.iter().any(|e| expr_references_names(e, names)),
        Expr::List(l) => l.elts.iter().any(|e| expr_references_names(e, names)),
        Expr::Set(s) => s.elts.iter().any(|e| expr_references_names(e, names)),
        Expr::Dict(d) => d.items.iter().any(|item| {
            item.key
                .as_ref()
                .is_some_and(|k| expr_references_names(k, names))
                || expr_references_names(&item.value, names)
        }),
        Expr::ListComp(c) => {
            expr_references_names(&c.elt, names)
                || c.generators
                    .iter()
                    .any(|g| comprehension_references_names(g, names))
        }
        Expr::SetComp(c) => {
            expr_references_names(&c.elt, names)
                || c.generators
                    .iter()
                    .any(|g| comprehension_references_names(g, names))
        }
        Expr::Generator(g) => {
            expr_references_names(&g.elt, names)
                || g.generators
                    .iter()
                    .any(|gen| comprehension_references_names(gen, names))
        }
        Expr::DictComp(d) => {
            d.key
                .as_ref()
                .is_some_and(|k| expr_references_names(k, names))
                || expr_references_names(&d.value, names)
                || d.generators
                    .iter()
                    .any(|g| comprehension_references_names(g, names))
        }
        Expr::Await(a) => expr_references_names(&a.value, names),
        Expr::Yield(y) => y
            .value
            .as_ref()
            .is_some_and(|v| expr_references_names(v, names)),
        Expr::YieldFrom(y) => expr_references_names(&y.value, names),
        Expr::Starred(s) => expr_references_names(&s.value, names),
        Expr::Slice(s) => {
            s.lower
                .as_ref()
                .is_some_and(|e| expr_references_names(e, names))
                || s.upper
                    .as_ref()
                    .is_some_and(|e| expr_references_names(e, names))
                || s.step
                    .as_ref()
                    .is_some_and(|e| expr_references_names(e, names))
        }
        Expr::FString(f) => f.value.elements().any(|el| match el {
            ruff_python_ast::InterpolatedStringElement::Interpolation(i) => {
                expr_references_names(&i.expression, names)
            }
            ruff_python_ast::InterpolatedStringElement::Literal(_) => false,
        }),
        Expr::Attribute(a) => expr_references_names(&a.value, names),
        // Leaf nodes that cannot contain the target names: literals, etc.
        _ => false,
    }
}

fn comprehension_references_names(gen: &ruff_python_ast::Comprehension, names: &[&str]) -> bool {
    expr_references_names(&gen.target, names)
        || expr_references_names(&gen.iter, names)
        || gen.ifs.iter().any(|i| expr_references_names(i, names))
}

/// Return `true` if `body` contains any reference to the `typhon_runtime`
/// module — either `import typhon_runtime` or `from typhon_runtime import …`.
/// When true, the build must still write the runtime helper file even if
/// no Ok/Err/Result names appear directly in expressions (the user may be
/// calling `typhon_runtime.Ok(...)` via the bare-import / qualified style).
fn has_any_typhon_runtime_import(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::Import(imp) => imp
            .names
            .iter()
            .any(|a| is_typhon_runtime_module(a.name.as_str())),
        Stmt::ImportFrom(imp) => imp
            .module
            .as_ref()
            .map(|m| is_typhon_runtime_module(m.as_str()))
            .unwrap_or(false),
        _ => false,
    })
}

/// True when `name` refers to the `typhon_runtime` package or one of its
/// submodules (`typhon_runtime.lazy`, `typhon_runtime.tasks`, …).
/// Matching only the bare package name would miss the `from
/// typhon_runtime.lazy import lazy_import as …` injection emitted for
/// `lazy import` lowering, leaving a lazy-import-only module without
/// the generated `build/typhon_runtime/` package and failing at
/// startup with `ModuleNotFoundError`. FINDINGS #84.
fn is_typhon_runtime_module(name: &str) -> bool {
    name == "typhon_runtime" || name.starts_with("typhon_runtime.")
}

/// Return `true` if an existing `from typhon_runtime import …` already brings
/// all three runtime names (`Ok`, `Err`, `Result`) into scope. Used to skip
/// injection only when the user-provided import is complete; partial imports
/// (e.g. just `Ok`) still need injection so the missing names resolve.
fn typhon_runtime_import_covers_all(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::ImportFrom(imp)
            if imp.module.as_ref().map(|m| m.as_str()) == Some("typhon_runtime") =>
        {
            let mut ok = false;
            let mut err = false;
            let mut result = false;
            for alias in &imp.names {
                match alias.name.as_str() {
                    "Ok" => ok = true,
                    "Err" => err = true,
                    "Result" => result = true,
                    "*" => return true,
                    _ => {}
                }
            }
            ok && err && result
        }
        _ => false,
    })
}

/// Build `from typhon_runtime import Ok, Err, Result`.
fn make_typhon_runtime_import() -> Stmt {
    Stmt::ImportFrom(StmtImportFrom {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        module: Some(make_identifier("typhon_runtime")),
        names: vec![make_alias("Ok"), make_alias("Err"), make_alias("Result")],
        level: 0,
        is_lazy: false,
    })
}

/// Build `import typhon_runtime`.
fn make_bare_typhon_runtime_import() -> Stmt {
    Stmt::Import(StmtImport {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        names: vec![make_alias("typhon_runtime")],
        is_lazy: false,
    })
}

/// Build `import asyncio`.
fn make_asyncio_import() -> Stmt {
    Stmt::Import(StmtImport {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        names: vec![make_alias("asyncio")],
        is_lazy: false,
    })
}

/// Build `import enum`.
fn make_enum_import() -> Stmt {
    Stmt::Import(StmtImport {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        names: vec![make_alias("enum")],
        is_lazy: false,
    })
}

/// `true` when `body` already has a bare `import enum` (no alias, or aliased to
/// `enum`). `import enum as e` doesn't satisfy the `enum.Enum` / `enum.auto()`
/// references produced by the `enum` lowering.
fn has_enum_import(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::Import(i) => i.names.iter().any(|a| {
            a.name.as_str() == "enum"
                && matches!(a.asname.as_ref().map(|n| n.as_str()), None | Some("enum"))
        }),
        _ => false,
    })
}

/// `true` when any class in `body` (recursively) inherits from an `enum.*`
/// base — the shape produced by the `enum Name:` lowering
/// (`class Name(enum.Enum):`). Drives `import enum` injection.
fn stmts_use_enum_base(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::ClassDef(c) => {
            c.bases().iter().any(is_enum_qualified_base) || stmts_use_enum_base(&c.body)
        }
        Stmt::If(s) => {
            stmts_use_enum_base(&s.body)
                || s.elif_else_clauses
                    .iter()
                    .any(|cl| stmts_use_enum_base(&cl.body))
        }
        Stmt::For(s) => stmts_use_enum_base(&s.body) || stmts_use_enum_base(&s.orelse),
        Stmt::While(s) => stmts_use_enum_base(&s.body) || stmts_use_enum_base(&s.orelse),
        Stmt::With(s) => stmts_use_enum_base(&s.body),
        Stmt::Try(s) => {
            stmts_use_enum_base(&s.body)
                || stmts_use_enum_base(&s.orelse)
                || stmts_use_enum_base(&s.finalbody)
        }
        Stmt::FunctionDef(s) => stmts_use_enum_base(&s.body),
        _ => false,
    })
}

/// `true` when any base of `c` is one of the standard enum bases (bare or
/// `enum.`-qualified). A `class! X(enum.StrEnum):` must NOT get a
/// synthesised `__init__` — enum bodies are member definitions, not
/// fields, and injecting a constructor (plus stripping the "defaults")
/// corrupts the EnumType machinery into a TypeError at import time.
fn class_has_enum_base(c: &ruff_python_ast::StmtClassDef) -> bool {
    c.bases().iter().any(|b| {
        is_enum_qualified_base(b)
            || matches!(
                base_last_segment(b),
                Some("Enum" | "IntEnum" | "StrEnum" | "Flag" | "IntFlag" | "ReprEnum")
            )
    })
}

/// `true` when `base` is a dotted `enum.<Family>` reference where `<Family>` is
/// one of the standard enum bases.
fn is_enum_qualified_base(base: &Expr) -> bool {
    if let Expr::Attribute(a) = base {
        if let Expr::Name(n) = a.value.as_ref() {
            return n.id.as_str() == "enum"
                && matches!(
                    a.attr.as_str(),
                    "Enum" | "IntEnum" | "StrEnum" | "Flag" | "IntFlag"
                );
        }
    }
    false
}

/// Rewrite the trailing `?` Typhon-nullable marker inside *quoted*
/// annotations (`items: "Sequence[int]?"`) to `| None`, in every
/// annotation position. Unquoted `T?` is handled by the preprocessor;
/// string literals escape it, and the raw `?` is a SyntaxError when the
/// runtime later evaluates the forward reference.
/// The rightmost identifier of a subscript head, so `Literal[...]`,
/// `typing.Literal[...]` and `t.Literal[...]` are all recognised.
fn subscript_head_name(value: &Expr) -> Option<String> {
    match value {
        Expr::Name(n) => Some(n.id.as_str().to_owned()),
        Expr::Attribute(a) => Some(a.attr.as_str().to_owned()),
        _ => None,
    }
}

fn normalise_quoted_annotation_nullability(body: &mut [Stmt]) {
    fn fix_expr(e: &mut Expr) {
        match e {
            Expr::StringLiteral(lit) => {
                let content = lit.value.to_str();
                let trimmed = content.trim_end();
                if let Some(inner) = trimmed.strip_suffix('?') {
                    let rewritten = format!("{} | None", inner.trim_end());
                    *e = make_string_literal_expr(&rewritten);
                }
            }
            Expr::Subscript(s) => {
                fix_expr(&mut s.value);
                // `Literal[...]` arguments are *values*, not type expressions:
                // `Literal["?"]` means the one-character string `"?"`, and
                // rewriting it to `Literal[" | None"]` silently changed the
                // constant. Same for `Annotated[T, meta]` past the first
                // argument, whose metadata is arbitrary and often a string.
                match subscript_head_name(&s.value).as_deref() {
                    Some("Literal") => {}
                    Some("Annotated") => {
                        if let Expr::Tuple(t) = s.slice.as_mut() {
                            if let Some(first) = t.elts.first_mut() {
                                fix_expr(first);
                            }
                        } else {
                            fix_expr(&mut s.slice);
                        }
                    }
                    _ => fix_expr(&mut s.slice),
                }
            }
            Expr::BinOp(b) => {
                fix_expr(&mut b.left);
                fix_expr(&mut b.right);
            }
            Expr::Tuple(t) => {
                for el in t.elts.iter_mut() {
                    fix_expr(el);
                }
            }
            _ => {}
        }
    }
    fn walk(stmts: &mut [Stmt]) {
        for stmt in stmts.iter_mut() {
            match stmt {
                Stmt::AnnAssign(a) => fix_expr(&mut a.annotation),
                Stmt::FunctionDef(f) => {
                    for p in f
                        .parameters
                        .posonlyargs
                        .iter_mut()
                        .chain(f.parameters.args.iter_mut())
                        .chain(f.parameters.kwonlyargs.iter_mut())
                    {
                        if let Some(ann) = p.parameter.annotation.as_mut() {
                            fix_expr(ann);
                        }
                    }
                    if let Some(r) = f.returns.as_mut() {
                        fix_expr(r);
                    }
                    walk(&mut f.body);
                }
                Stmt::ClassDef(c) => walk(&mut c.body),
                Stmt::If(s) => {
                    walk(&mut s.body);
                    for cl in s.elif_else_clauses.iter_mut() {
                        walk(&mut cl.body);
                    }
                }
                Stmt::While(s) => walk(&mut s.body),
                Stmt::For(s) => walk(&mut s.body),
                Stmt::With(s) => walk(&mut s.body),
                Stmt::Try(t) => {
                    walk(&mut t.body);
                    for h in t.handlers.iter_mut() {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                        walk(&mut h.body);
                    }
                    walk(&mut t.orelse);
                    walk(&mut t.finalbody);
                }
                _ => {}
            }
        }
    }
    walk(body);
}

/// The `collections.abc` names the checker's prelude accepts unimported.
const ABC_PRELUDE_NAMES: &[&str] = &[
    "Callable",
    "Iterable",
    "Iterator",
    "Generator",
    "AsyncIterable",
    "AsyncIterator",
    "AsyncGenerator",
    "Awaitable",
    "Coroutine",
    "Sequence",
    "MutableSequence",
    "Mapping",
    "MutableMapping",
    "MutableSet",
    "Collection",
    "Container",
    "Reversible",
    "Hashable",
    "Sized",
    "KeysView",
    "ValuesView",
    "ItemsView",
];

/// Names bound at module level (imports, defs, classes, assignments) —
/// anything here doesn't need an injected import.
fn module_level_bound_names<'a>(body: &'a [Stmt]) -> HashSet<&'a str> {
    let mut bound: HashSet<&'a str> = HashSet::new();
    for stmt in body {
        match stmt {
            Stmt::Import(i) => {
                for a in &i.names {
                    let n = a
                        .asname
                        .as_ref()
                        .map(|n| n.as_str())
                        .unwrap_or_else(|| a.name.as_str().split('.').next().unwrap_or(""));
                    bound.insert(n);
                }
            }
            Stmt::ImportFrom(i) => {
                for a in &i.names {
                    let n = a
                        .asname
                        .as_ref()
                        .map(|n| n.as_str())
                        .unwrap_or_else(|| a.name.as_str());
                    bound.insert(n);
                }
            }
            Stmt::FunctionDef(f) => {
                bound.insert(f.name.as_str());
            }
            Stmt::ClassDef(c) => {
                bound.insert(c.name.as_str());
            }
            Stmt::Assign(a) => {
                for t in &a.targets {
                    if let Expr::Name(n) = t {
                        bound.insert(n.id.as_str());
                    }
                }
            }
            Stmt::AnnAssign(a) => {
                if let Expr::Name(n) = a.target.as_ref() {
                    bound.insert(n.id.as_str());
                }
            }
            Stmt::TypeAlias(ta) => {
                if let Expr::Name(n) = ta.name.as_ref() {
                    bound.insert(n.id.as_str());
                }
            }
            _ => {}
        }
    }
    bound
}

/// Collect every `collections.abc` prelude name referenced from an
/// annotation position anywhere in the module that is NOT bound at module
/// level. Returned in `ABC_PRELUDE_NAMES` order (deterministic emit).
fn collect_unimported_abc_annotation_names(body: &[Stmt]) -> Vec<&'static str> {
    let bound = module_level_bound_names(body);
    let mut used: HashSet<&'static str> = HashSet::new();

    fn names_in_annotation(e: &Expr, used: &mut HashSet<&'static str>) {
        match e {
            Expr::Name(n) => {
                if let Some(canon) = ABC_PRELUDE_NAMES.iter().find(|c| **c == n.id.as_str()) {
                    used.insert(canon);
                }
            }
            Expr::Subscript(s) => {
                names_in_annotation(&s.value, used);
                names_in_annotation(&s.slice, used);
            }
            Expr::BinOp(b) => {
                names_in_annotation(&b.left, used);
                names_in_annotation(&b.right, used);
            }
            Expr::Tuple(t) => {
                for el in &t.elts {
                    names_in_annotation(el, used);
                }
            }
            Expr::List(l) => {
                for el in &l.elts {
                    names_in_annotation(el, used);
                }
            }
            // Quoted forward references (`x: "Iterator[int]"`) are
            // resolved by the checker and evaluated at runtime by
            // `typing.get_type_hints` — the names inside still need the
            // import. Word-boundary scan of the literal's content.
            Expr::StringLiteral(lit) => {
                let content = lit.value.to_str();
                for canon in ABC_PRELUDE_NAMES {
                    let mut rest = content;
                    while let Some(pos) = rest.find(canon) {
                        let before_ok = pos == 0
                            || !rest[..pos]
                                .chars()
                                .next_back()
                                .is_some_and(|c| c.is_alphanumeric() || c == '_');
                        let after = &rest[pos + canon.len()..];
                        let after_ok = !after
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_alphanumeric() || c == '_');
                        if before_ok && after_ok {
                            used.insert(canon);
                            break;
                        }
                        rest = &rest[pos + canon.len()..];
                    }
                }
            }
            // Attribute access (`typing.Iterator`) is already qualified —
            // no injection needed.
            _ => {}
        }
    }

    fn walk(stmts: &[Stmt], used: &mut HashSet<&'static str>) {
        for stmt in stmts {
            match stmt {
                Stmt::FunctionDef(f) => {
                    for p in f
                        .parameters
                        .posonlyargs
                        .iter()
                        .chain(f.parameters.args.iter())
                        .chain(f.parameters.kwonlyargs.iter())
                    {
                        if let Some(ann) = &p.parameter.annotation {
                            names_in_annotation(ann, used);
                        }
                    }
                    for p in [&f.parameters.vararg, &f.parameters.kwarg]
                        .into_iter()
                        .flatten()
                    {
                        if let Some(ann) = &p.annotation {
                            names_in_annotation(ann, used);
                        }
                    }
                    if let Some(r) = &f.returns {
                        names_in_annotation(r, used);
                    }
                    walk(&f.body, used);
                }
                Stmt::ClassDef(c) => walk(&c.body, used),
                Stmt::AnnAssign(a) => names_in_annotation(&a.annotation, used),
                Stmt::If(s) => {
                    walk(&s.body, used);
                    for cl in &s.elif_else_clauses {
                        walk(&cl.body, used);
                    }
                }
                Stmt::While(s) => walk(&s.body, used),
                Stmt::For(s) => walk(&s.body, used),
                Stmt::With(s) => walk(&s.body, used),
                Stmt::Try(t) => {
                    walk(&t.body, used);
                    for h in &t.handlers {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                        walk(&h.body, used);
                    }
                    walk(&t.orelse, used);
                    walk(&t.finalbody, used);
                }
                Stmt::Match(m) => {
                    for case in &m.cases {
                        walk(&case.body, used);
                    }
                }
                _ => {}
            }
        }
    }

    walk(body, &mut used);
    ABC_PRELUDE_NAMES
        .iter()
        .filter(|n| used.contains(*n) && !bound.contains(**n))
        .copied()
        .collect()
}

/// Build `from collections.abc import <names...>`.
fn make_collections_abc_import(names: &[&'static str]) -> Stmt {
    Stmt::ImportFrom(StmtImportFrom {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        module: Some(make_identifier("collections.abc")),
        names: names.iter().map(|n| make_alias(n)).collect(),
        level: 0,
        is_lazy: false,
    })
}

/// Build `from typing import Protocol`.
fn make_protocol_import() -> Stmt {
    Stmt::ImportFrom(StmtImportFrom {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        module: Some(make_identifier("typing")),
        names: vec![make_alias("Protocol")],
        level: 0,
        is_lazy: false,
    })
}

fn make_newtype_import() -> Stmt {
    Stmt::ImportFrom(StmtImportFrom {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        module: Some(make_identifier("typing")),
        names: vec![make_alias("NewType")],
        level: 0,
        is_lazy: false,
    })
}

fn make_freeze_import() -> Stmt {
    Stmt::ImportFrom(StmtImportFrom {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        module: Some(make_identifier("typhon_runtime.freeze")),
        names: vec![ruff_python_ast::Alias {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            name: make_identifier("deep_freeze"),
            asname: Some(make_identifier("__typhon_freeze__")),
        }],
        level: 0,
        is_lazy: false,
    })
}

/// Build `from typhon_runtime import try_result`.
fn make_try_result_import() -> Stmt {
    Stmt::ImportFrom(StmtImportFrom {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        module: Some(make_identifier("typhon_runtime")),
        names: vec![ruff_python_ast::Alias {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            name: make_identifier("try_result"),
            asname: None,
        }],
        level: 0,
        is_lazy: false,
    })
}

/// Return `true` if an existing `from typhon_runtime import …` already brings
/// `try_result` into scope, so the injector doesn't duplicate it.
fn has_try_result_import(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::ImportFrom(i) => {
            i.module.as_ref().map(|m| m.as_str()) == Some("typhon_runtime")
                && i.names.iter().any(|a| a.name.as_str() == "try_result")
        }
        _ => false,
    })
}

/// Build the two statements injected at the top of the entry module's
/// `if __name__ == "__main__":` block when `[emit] traceback-remap` is on:
///
/// ```python
///     from typhon_runtime.traceback import install as __typhon_install_tb__
///     __typhon_install_tb__()
/// ```
///
/// The installed `sys.excepthook` rewrites an uncaught exception's traceback
/// to point at `.ty` source via the emitted `.py.map` sidecars — the same
/// mapping `tyc trace` applies, but automatically and only for the entry
/// script (library imports never trip the `__main__` guard).
fn make_traceback_install_stmts() -> Vec<Stmt> {
    let import = Stmt::ImportFrom(StmtImportFrom {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        module: Some(make_identifier("typhon_runtime.traceback")),
        names: vec![Alias {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            name: make_identifier("install"),
            asname: Some(make_identifier("__typhon_install_tb__")),
        }],
        level: 0,
        is_lazy: false,
    });
    let call = Expr::Call(ExprCall {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        func: Box::new(Expr::Name(ExprName {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            id: Name::new("__typhon_install_tb__"),
            ctx: ExprContext::Load,
        })),
        arguments: Arguments {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            args: Box::new([]),
            keywords: Box::new([]),
        },
    });
    let call_stmt = Stmt::Expr(ast::StmtExpr {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        value: Box::new(call),
    });
    vec![import, call_stmt]
}

/// True when `test` is the `__name__ == "__main__"` entry-point guard.
fn is_main_guard(test: &Expr) -> bool {
    let Expr::Compare(cmp) = test else {
        return false;
    };
    let Expr::Name(left) = cmp.left.as_ref() else {
        return false;
    };
    if left.id.as_str() != "__name__" || cmp.ops.len() != 1 {
        return false;
    }
    if !matches!(cmp.ops[0], ast::CmpOp::Eq) {
        return false;
    }
    matches!(
        cmp.comparators.first(),
        Some(Expr::StringLiteral(s)) if s.value.to_str() == "__main__"
    )
}

/// Inject the traceback-installer statements at the top of the first
/// top-level `if __name__ == "__main__":` block. Returns `true` when an
/// injection was made (so the caller flags `needs_typhon_runtime`).
fn inject_traceback_install(body: &mut [Stmt]) -> bool {
    for stmt in body.iter_mut() {
        if let Stmt::If(if_stmt) = stmt {
            if is_main_guard(&if_stmt.test) {
                let mut stmts = make_traceback_install_stmts();
                stmts.append(&mut if_stmt.body);
                if_stmt.body = stmts;
                return true;
            }
        }
    }
    false
}

/// `true` when `body` already binds `__typhon_freeze__` as an alias of
/// `typhon_runtime.freeze.deep_freeze`.
fn has_freeze_import(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::ImportFrom(i) => {
            i.module.as_ref().map(|m| m.as_str()) == Some("typhon_runtime.freeze")
                && i.names.iter().any(|a| {
                    a.name.as_str() == "deep_freeze"
                        && a.asname.as_ref().map(|n| n.as_str()) == Some("__typhon_freeze__")
                })
        }
        _ => false,
    })
}

/// `true` when `body` has at least one expression of the form
/// `__typhon_freeze__(...)` — the desugared shape of `freeze let`.
fn stmts_use_freeze_call(body: &[Stmt]) -> bool {
    body.iter().any(stmt_uses_freeze_call)
}

fn stmt_uses_freeze_call(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Assign(a) => expr_uses_freeze_call(&a.value),
        Stmt::AnnAssign(a) => a.value.as_ref().is_some_and(|v| expr_uses_freeze_call(v)),
        Stmt::FunctionDef(f) => stmts_use_freeze_call(&f.body),
        Stmt::ClassDef(c) => stmts_use_freeze_call(&c.body),
        Stmt::If(s) => {
            stmts_use_freeze_call(&s.body)
                || s.elif_else_clauses
                    .iter()
                    .any(|c| stmts_use_freeze_call(&c.body))
        }
        _ => false,
    }
}

fn expr_uses_freeze_call(expr: &Expr) -> bool {
    if let Expr::Call(c) = expr {
        if let Expr::Name(n) = c.func.as_ref() {
            if n.id.as_str() == "__typhon_freeze__" {
                return true;
            }
        }
    }
    false
}

/// Synthesise `__all__ = ["a", "b", ...]` from the list of `pub`
/// names. Emitted as a regular module-level assignment so downstream
/// tooling sees a plain Python `__all__` declaration.
fn make_dunder_all(names: &[String]) -> Stmt {
    let elts: Vec<Expr> = names.iter().map(|n| make_string_literal_expr(n)).collect();
    let list_expr = Expr::List(ruff_python_ast::ExprList {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        elts,
        ctx: ExprContext::Load,
    });
    Stmt::Assign(StmtAssign {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        targets: vec![Expr::Name(ExprName {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            id: ruff_python_ast::name::Name::new_static("__all__"),
            ctx: ExprContext::Store,
        })],
        value: Box::new(list_expr),
        mutability: None,
    })
}

/// `true` when `stmt` is a hand-written `__all__ = ...` assignment.
/// Used by the desugar pass to keep author-supplied lists untouched
/// even when `pub` declarations are present.
fn is_dunder_all_assignment(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Assign(a) => a
            .targets
            .iter()
            .any(|t| matches!(t, Expr::Name(n) if n.id.as_str() == "__all__")),
        Stmt::AnnAssign(a) => {
            matches!(a.target.as_ref(), Expr::Name(n) if n.id.as_str() == "__all__")
        }
        _ => false,
    }
}

/// `true` when `body` already binds the bare name `NewType` from `typing`.
fn has_newtype_import(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::ImportFrom(i) => {
            i.module.as_ref().map(|m| m.as_str()) == Some("typing")
                && i.names.iter().any(|a| match a.name.as_str() {
                    "NewType" => matches!(
                        a.asname.as_ref().map(|n| n.as_str()),
                        None | Some("NewType")
                    ),
                    "*" => true,
                    _ => false,
                })
        }
        _ => false,
    })
}

/// `true` when `body` has at least one module-level assignment of the form
/// `Name = NewType("Name", Base)`. The desugar pass uses this to decide
/// whether `from typing import NewType` needs to be injected.
fn stmts_use_newtype_call(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::Assign(a) => {
            if a.targets.len() != 1 {
                return false;
            }
            let Expr::Name(target_name) = &a.targets[0] else {
                return false;
            };
            let Expr::Call(call) = a.value.as_ref() else {
                return false;
            };
            let Expr::Name(callee) = call.func.as_ref() else {
                return false;
            };
            if callee.id.as_str() != "NewType" {
                return false;
            }
            let Some(first_arg) = call.arguments.args.first() else {
                return false;
            };
            let Expr::StringLiteral(s) = first_arg else {
                return false;
            };
            s.value.to_str() == target_name.id.as_str()
        }
        _ => false,
    })
}

/// `true` when `body` binds the bare module name `asyncio`. The `gather`
/// lowering emits `asyncio.TaskGroup` / `asyncio.gather` against that exact
/// name; an `import asyncio as aio` doesn't satisfy the reference.
fn has_asyncio_import(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::Import(i) => i.names.iter().any(|a| {
            a.name.as_str() == "asyncio"
                && matches!(
                    a.asname.as_ref().map(|n| n.as_str()),
                    None | Some("asyncio")
                )
        }),
        _ => false,
    })
}

/// `true` when `body` binds the bare name `Protocol` (the `interface` lowering
/// emits `class Foo(Protocol):` against the unaliased name).
/// `from typing import Protocol as P` doesn't satisfy this; `from typing
/// import *` does.
fn has_protocol_import(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::ImportFrom(i) => {
            i.module.as_ref().map(|m| m.as_str()) == Some("typing")
                && i.names.iter().any(|a| match a.name.as_str() {
                    "Protocol" => matches!(
                        a.asname.as_ref().map(|n| n.as_str()),
                        None | Some("Protocol")
                    ),
                    "*" => true,
                    _ => false,
                })
        }
        _ => false,
    })
}

/// Return `true` if `body` already has a bare `import typhon_runtime`. The
/// lowered `go` / `lazy` expressions need the module bound under that exact
/// name; an `import typhon_runtime as tr` doesn't satisfy that.
fn has_bare_typhon_runtime_import(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::Import(i) => i.names.iter().any(|a| {
            a.name.as_str() == "typhon_runtime"
                && matches!(
                    a.asname.as_ref().map(|n| n.as_str()),
                    None | Some("typhon_runtime")
                )
        }),
        _ => false,
    })
}

/// Walk every expression in `stmts` and return `true` if any `typhon_runtime.<…>`
/// attribute access appears (e.g. `typhon_runtime.tasks.spawn(...)`).
fn expr_tree_uses_runtime_attribute(stmts: &[Stmt]) -> bool {
    stmts.iter().any(stmt_uses_runtime_attribute)
}

fn stmt_uses_runtime_attribute(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Expr(e) => expr_uses_runtime_attribute(&e.value),
        Stmt::Assign(a) => {
            expr_uses_runtime_attribute(&a.value)
                || a.targets.iter().any(expr_uses_runtime_attribute)
        }
        Stmt::AnnAssign(a) => {
            a.value
                .as_ref()
                .is_some_and(|v| expr_uses_runtime_attribute(v))
                || expr_uses_runtime_attribute(&a.target)
        }
        Stmt::AugAssign(a) => {
            expr_uses_runtime_attribute(&a.target) || expr_uses_runtime_attribute(&a.value)
        }
        Stmt::Return(r) => r
            .value
            .as_ref()
            .is_some_and(|v| expr_uses_runtime_attribute(v)),
        Stmt::FunctionDef(f) => expr_tree_uses_runtime_attribute(&f.body),
        Stmt::ClassDef(c) => expr_tree_uses_runtime_attribute(&c.body),
        Stmt::If(i) => {
            expr_uses_runtime_attribute(&i.test)
                || expr_tree_uses_runtime_attribute(&i.body)
                || i.elif_else_clauses.iter().any(|c| {
                    c.test.as_ref().is_some_and(expr_uses_runtime_attribute)
                        || expr_tree_uses_runtime_attribute(&c.body)
                })
        }
        Stmt::While(w) => {
            expr_uses_runtime_attribute(&w.test)
                || expr_tree_uses_runtime_attribute(&w.body)
                || expr_tree_uses_runtime_attribute(&w.orelse)
        }
        Stmt::For(f) => {
            expr_uses_runtime_attribute(&f.iter)
                || expr_tree_uses_runtime_attribute(&f.body)
                || expr_tree_uses_runtime_attribute(&f.orelse)
        }
        Stmt::With(w) => expr_tree_uses_runtime_attribute(&w.body),
        Stmt::Try(t) => {
            expr_tree_uses_runtime_attribute(&t.body)
                || expr_tree_uses_runtime_attribute(&t.orelse)
                || expr_tree_uses_runtime_attribute(&t.finalbody)
        }
        Stmt::Match(m) => {
            expr_uses_runtime_attribute(&m.subject)
                || m.cases
                    .iter()
                    .any(|case| expr_tree_uses_runtime_attribute(&case.body))
        }
        _ => false,
    }
}

fn expr_uses_runtime_attribute(expr: &Expr) -> bool {
    match expr {
        Expr::Attribute(a) => {
            // Root attribute chain: `typhon_runtime.<x>.<y>...`
            if let Expr::Name(n) = a.value.as_ref() {
                if n.id.as_str() == "typhon_runtime" {
                    return true;
                }
            }
            expr_uses_runtime_attribute(&a.value)
        }
        Expr::Call(c) => {
            expr_uses_runtime_attribute(&c.func)
                || c.arguments.args.iter().any(expr_uses_runtime_attribute)
                || c.arguments
                    .keywords
                    .iter()
                    .any(|k| expr_uses_runtime_attribute(&k.value))
        }
        Expr::Await(a) => expr_uses_runtime_attribute(&a.value),
        Expr::BinOp(b) => {
            expr_uses_runtime_attribute(&b.left) || expr_uses_runtime_attribute(&b.right)
        }
        Expr::UnaryOp(u) => expr_uses_runtime_attribute(&u.operand),
        Expr::If(i) => {
            expr_uses_runtime_attribute(&i.test)
                || expr_uses_runtime_attribute(&i.body)
                || expr_uses_runtime_attribute(&i.orelse)
        }
        Expr::Tuple(t) => t.elts.iter().any(expr_uses_runtime_attribute),
        Expr::List(l) => l.elts.iter().any(expr_uses_runtime_attribute),
        Expr::Lambda(l) => expr_uses_runtime_attribute(&l.body),
        _ => false,
    }
}

/// Return `true` if any class in `body` inherits from `Protocol` (Typhon's
/// `interface` lowering).
fn stmts_use_protocol_base(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::ClassDef(c) => {
            c.bases()
                .iter()
                .any(|b| matches!(b, Expr::Name(n) if n.id.as_str() == "Protocol"))
                || stmts_use_protocol_base(&c.body)
        }
        Stmt::FunctionDef(f) => stmts_use_protocol_base(&f.body),
        Stmt::If(s) => {
            stmts_use_protocol_base(&s.body)
                || s.elif_else_clauses
                    .iter()
                    .any(|c| stmts_use_protocol_base(&c.body))
        }
        _ => false,
    })
}

/// Return `true` if any expression in `body` references the `asyncio` module
/// by qualified attribute access (`asyncio.TaskGroup`, `asyncio.gather`, …).
fn stmts_use_asyncio_qualified(body: &[Stmt]) -> bool {
    body.iter().any(stmt_uses_asyncio_qualified)
}

fn stmt_uses_asyncio_qualified(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Expr(e) => expr_uses_asyncio_qualified(&e.value),
        Stmt::Assign(a) => {
            expr_uses_asyncio_qualified(&a.value)
                || a.targets.iter().any(expr_uses_asyncio_qualified)
        }
        Stmt::AnnAssign(a) => a
            .value
            .as_ref()
            .is_some_and(|v| expr_uses_asyncio_qualified(v)),
        Stmt::AugAssign(a) => {
            expr_uses_asyncio_qualified(&a.target) || expr_uses_asyncio_qualified(&a.value)
        }
        Stmt::Return(r) => r
            .value
            .as_ref()
            .is_some_and(|v| expr_uses_asyncio_qualified(v)),
        Stmt::With(w) => {
            w.items
                .iter()
                .any(|i| expr_uses_asyncio_qualified(&i.context_expr))
                || stmts_use_asyncio_qualified(&w.body)
        }
        Stmt::FunctionDef(f) => stmts_use_asyncio_qualified(&f.body),
        Stmt::ClassDef(c) => stmts_use_asyncio_qualified(&c.body),
        Stmt::If(i) => {
            expr_uses_asyncio_qualified(&i.test)
                || stmts_use_asyncio_qualified(&i.body)
                || i.elif_else_clauses.iter().any(|c| {
                    c.test.as_ref().is_some_and(expr_uses_asyncio_qualified)
                        || stmts_use_asyncio_qualified(&c.body)
                })
        }
        Stmt::While(w) => {
            stmts_use_asyncio_qualified(&w.body) || stmts_use_asyncio_qualified(&w.orelse)
        }
        Stmt::For(f) => {
            expr_uses_asyncio_qualified(&f.iter)
                || stmts_use_asyncio_qualified(&f.body)
                || stmts_use_asyncio_qualified(&f.orelse)
        }
        Stmt::Try(t) => {
            stmts_use_asyncio_qualified(&t.body)
                || t.handlers.iter().any(|h| {
                    let ExceptHandler::ExceptHandler(h) = h;
                    stmts_use_asyncio_qualified(&h.body)
                })
                || stmts_use_asyncio_qualified(&t.orelse)
                || stmts_use_asyncio_qualified(&t.finalbody)
        }
        // A `gather:` lowered to `async with asyncio.TaskGroup()` can sit
        // inside a `case` arm; without descending into match arms the
        // `import asyncio` injection was skipped and the emitted module
        // raised `NameError` at runtime (kilnlog #3). Guards can also carry
        // qualified `asyncio.*` calls.
        Stmt::Match(m) => {
            expr_uses_asyncio_qualified(&m.subject)
                || m.cases.iter().any(|case| {
                    case.guard
                        .as_ref()
                        .is_some_and(|g| expr_uses_asyncio_qualified(g))
                        || stmts_use_asyncio_qualified(&case.body)
                })
        }
        _ => false,
    }
}

fn expr_uses_asyncio_qualified(expr: &Expr) -> bool {
    match expr {
        Expr::Attribute(a) => {
            if let Expr::Name(n) = a.value.as_ref() {
                if n.id.as_str() == "asyncio" {
                    return true;
                }
            }
            expr_uses_asyncio_qualified(&a.value)
        }
        Expr::Call(c) => {
            expr_uses_asyncio_qualified(&c.func)
                || c.arguments.args.iter().any(expr_uses_asyncio_qualified)
                || c.arguments
                    .keywords
                    .iter()
                    .any(|k| expr_uses_asyncio_qualified(&k.value))
        }
        Expr::Await(a) => expr_uses_asyncio_qualified(&a.value),
        Expr::BinOp(b) => {
            expr_uses_asyncio_qualified(&b.left) || expr_uses_asyncio_qualified(&b.right)
        }
        Expr::UnaryOp(u) => expr_uses_asyncio_qualified(&u.operand),
        Expr::Tuple(t) => t.elts.iter().any(expr_uses_asyncio_qualified),
        _ => false,
    }
}

// ── module-level desugaring ──────────────────────────────────────────────────

fn desugar_mod_module_with(m: &ModModule, options: &DesugarOptions) -> ModModule {
    // Lower TypedDict-style literals against local class/model annotations
    // (`let u: User = {"id": 1, "name": "ada"}`) into constructor calls
    // (`User(id=1, name="ada")`). The type checker has accepted this form
    // since v0.3.0, but without this rewrite the emitted Python kept the
    // plain dict — `u.name` then crashed at runtime with AttributeError.
    let mut body: Vec<Stmt> = m.body.clone();
    {
        let class_fields = collect_local_class_field_annotations(&body);
        if !class_fields.is_empty() {
            rewrite_typed_dict_literals_in_stmts(&mut body, &class_fields);
        }
    }
    // Quoted annotations are forward references; Typhon's `?` suffix can
    // survive inside the string (the preprocessor can't see into
    // literals). Normalise `"T?"` to `"T | None"` so runtime annotation
    // evaluation (`typing.get_type_hints`, pydantic, FastAPI) parses it.
    normalise_quoted_annotation_nullability(&mut body);
    let m = &ModModule {
        range: m.range,
        node_index: ruff_python_ast::AtomicNodeIndex::NONE,
        body,
    };
    let mut multi_base_parents = collect_multi_base_parents(&m.body);
    // Classes whose impl-stub body contains a `cached_property` method (or
    // the aliased `_typhon_cached_property` form emitted by `lazy let` in
    // an impl block) need `__dict__` to back the property cache; treat them
    // as multi-base parents so the dataclass decorator drops `slots=True`.
    collect_cached_property_targets_into(&m.body, &mut multi_base_parents);
    // Module-level classes and the transitive exception subset among them.
    let mut module_level_classes: Vec<(String, Vec<String>)> = Vec::new();
    collect_class_bases_into(&m.body, &mut module_level_classes);
    let module_class_names: std::collections::HashSet<String> = module_level_classes
        .iter()
        .map(|(n, _)| n.clone())
        .collect();
    let exception_class_names =
        exception_class_names_from(&module_level_classes, &module_class_names);
    let markers = ClassMarkers {
        raw_starts: &options.raw_class_line_starts,
        frozen_starts: &options.frozen_class_line_starts,
        plain_starts: &options.plain_class_line_starts,
        multi_base_parents: &multi_base_parents,
        skip_decoration_bases: &options.skip_decoration_bases,
        model_extra: &options.model_extra,
        exception_class_names: &exception_class_names,
        module_class_names: &module_class_names,
    };
    let (new_body, transformed_classes) = desugar_stmts(&m.body, markers);

    // Merge `impl` pseudo-classes into their target classes and remove the stubs.
    let (merged_body, _) = merge_impl_blocks(new_body);

    // H0: rewrite bare zero-argument `super()` inside methods into the explicit
    // two-argument `super(EnclosingClass, self)` form. The bare form relies on
    // the `__class__` closure cell, which `@dataclass(slots=True)` orphans when
    // it rebuilds the class object — so `super()` crashes at runtime. The
    // two-arg form does not depend on `__class__`. Runs after `merge_impl_blocks`
    // so methods brought in from `impl`/`extend` blocks (already inside their
    // target class) are covered too.
    let merged_body = rewrite_bare_super(merged_body);

    // FINDINGS #22: propagate parent field annotations into subclass bodies
    // so the VM's auto-`__init__` synthesiser (which reads `class.fields`
    // directly from class-body `AnnAssign` nodes) accepts inherited fields
    // as kwargs. The compile path runs through CPython's `@dataclass(slots
    // =True)` decorator, which already walks the MRO when collecting
    // fields, so no transformation is needed there — but the duplicated
    // annotation is harmless under `@dataclass` and keeps the desugared
    // module consistent across run modes.
    let merged_body = inherit_parent_fields(merged_body);

    // Inject `@functools.cache` on every top-level function name the purity
    // analyser flagged as opted-into memoisation.  Returns whether any cache
    // decorator was added so we can also inject `import functools` once.
    let (with_caches, added_cache) =
        inject_memoise_decorators(merged_body, &options.memoise_functions);

    let needs_dataclasses = transformed_classes && !has_dataclasses_import(&m.body);
    let needs_functools = added_cache && !has_functools_import(&with_caches);

    let mut final_body = with_caches;
    let insert_at = import_insert_pos(&final_body);
    if needs_functools {
        final_body.insert(insert_at, make_functools_import());
    }
    if needs_dataclasses {
        final_body.insert(insert_at, make_dataclasses_import());
    }

    ModModule {
        range: m.range,
        node_index: AtomicNodeIndex::NONE,
        body: final_body,
    }
}

/// Strip Typhon-internal `@pure` / `@memo` decorators wherever they appear
/// (top-level functions, async functions, class methods, nested functions)
/// and prepend `@functools.cache` to every TOP-LEVEL function whose name
/// appears in `memoise`. Returns the modified body and whether any cache
/// decorator was inserted.
///
/// Memoise injection is intentionally restricted to top-level functions:
/// the purity analyser only collects top-level findings (so a nested `@memo
/// def f` doesn't accidentally trigger injection on an unrelated top-level
/// `def f`). Stripping of `@pure` / `@memo` markers, by contrast, recurses
/// everywhere — otherwise those Typhon-only names would leak into the
/// emitted Python and raise `NameError` at import time.
fn inject_memoise_decorators(body: Vec<Stmt>, memoise: &[String]) -> (Vec<Stmt>, bool) {
    let mut added = false;
    // Names this module binds for itself — a user or third-party `pure` /
    // `memo` / `gatherable` is not Typhon's marker and must survive.
    let shadowed = user_bound_marker_names(&body);
    let new_body: Vec<Stmt> = body
        .into_iter()
        .map(|stmt| match stmt {
            Stmt::FunctionDef(mut f) => {
                f.decorator_list = strip_purity_decorators_with(f.decorator_list, &shadowed);
                f.body = strip_purity_decorators_in_body_with(f.body, &shadowed);
                if memoise.iter().any(|n| n == f.name.as_str())
                    && !has_cache_decorator(&f.decorator_list)
                {
                    f.decorator_list
                        .insert(0, make_functools_dot_cache_decorator());
                    added = true;
                }
                Stmt::FunctionDef(f)
            }
            Stmt::ClassDef(mut c) => {
                c.body = strip_purity_decorators_in_body_with(c.body, &shadowed);
                Stmt::ClassDef(c)
            }
            other => other,
        })
        .collect();
    (new_body, added)
}

/// Recursively strip `@pure` / `@memo` markers from every function defined
/// inside `body` (and any nested function/class bodies). Used to clean up
/// Typhon-only decorators that the top-level pass doesn't see directly —
/// class methods and nested functions.
fn strip_purity_decorators_in_body_with(
    body: Vec<Stmt>,
    shadowed: &std::collections::HashSet<String>,
) -> Vec<Stmt> {
    body.into_iter()
        .map(|stmt| match stmt {
            Stmt::FunctionDef(mut f) => {
                f.decorator_list = strip_purity_decorators_with(f.decorator_list, shadowed);
                f.body = strip_purity_decorators_in_body_with(f.body, shadowed);
                Stmt::FunctionDef(f)
            }
            Stmt::ClassDef(mut c) => {
                c.body = strip_purity_decorators_in_body_with(c.body, shadowed);
                Stmt::ClassDef(c)
            }
            other => other,
        })
        .collect()
}

// `user_bound_marker_names` — the "is this really Typhon's `@pure` /
// `@memo` / `@gatherable` marker, or a name the module owns?" derivation —
// is shared with `tyc-analyse` via `tyc-syntax` (the same rule-of-one home
// as `mro::field_collection_order`): the analyser and this crate make
// correlated decisions about the same three names, so a private copy here
// was a drift hazard.
use tyc_syntax::user_bound_marker_names;

/// Drop `@pure`, `@pure(...)`, and `@memo` decorators from a function — they
/// are Typhon-only metadata, not actual Python runtime decorators.
fn strip_purity_decorators_with(
    decorators: Vec<Decorator>,
    shadowed: &std::collections::HashSet<String>,
) -> Vec<Decorator> {
    decorators
        .into_iter()
        .filter(|d| !is_purity_marker_with(&d.expression, shadowed))
        .collect()
}

fn is_purity_marker_with(d: &Expr, shadowed: &std::collections::HashSet<String>) -> bool {
    match d {
        // `gatherable` lives alongside `pure` / `memo` as a Typhon-internal
        // attestation: the user marks `async def fetch_user(...)` with it
        // to opt the function in as an auto-gather candidate, but the name
        // has no Python runtime form so the emitter must drop it.
        Expr::Name(n) => {
            matches!(n.id.as_str(), "pure" | "memo" | "gatherable")
                && !shadowed.contains(n.id.as_str())
        }
        Expr::Call(c) => is_purity_marker_with(&c.func, shadowed),
        _ => false,
    }
}

fn is_cache_decorator_expr(expr: &Expr) -> bool {
    match expr {
        Expr::Name(n) => matches!(n.id.as_str(), "cache" | "lru_cache"),
        Expr::Attribute(a) => {
            if let Expr::Name(n) = a.value.as_ref() {
                n.id.as_str() == "functools" && matches!(a.attr.as_str(), "cache" | "lru_cache")
            } else {
                false
            }
        }
        Expr::Call(c) => is_cache_decorator_expr(c.func.as_ref()),
        _ => false,
    }
}

fn has_cache_decorator(decorators: &[Decorator]) -> bool {
    decorators
        .iter()
        .any(|d| is_cache_decorator_expr(&d.expression))
}

/// `true` when `body` binds the bare module name `functools` (so the
/// `@functools.cache` decorator the desugarer injects can resolve it).
///
/// Aliased imports (`import functools as ft`) and from-imports
/// (`from functools import cache`) intentionally do NOT count: neither
/// binds the name `functools` itself, so the injected reference would
/// raise `NameError` at import time.
fn has_functools_import(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::Import(i) => i.names.iter().any(|a| {
            a.name.as_str() == "functools"
                && matches!(
                    a.asname.as_ref().map(|n| n.as_str()),
                    None | Some("functools")
                )
        }),
        _ => false,
    })
}

fn make_functools_import() -> Stmt {
    Stmt::Import(StmtImport {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        names: vec![make_alias("functools")],
        is_lazy: false,
    })
}

/// Build the decorator `@functools.cache` (as a `Decorator` node).
fn make_functools_dot_cache_decorator() -> Decorator {
    let functools_name = Expr::Name(ExprName {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        id: Name::new("functools"),
        ctx: ExprContext::Load,
    });
    let expr = Expr::Attribute(ExprAttribute {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        value: Box::new(functools_name),
        attr: make_identifier("cache"),
        ctx: ExprContext::Load,
    });
    Decorator {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        expression: expr,
    }
}

/// Return the index at which a new top-level import should be inserted,
/// skipping past an optional module docstring and any `from __future__ import`
/// statements (both must remain at the top of a Python module).
fn import_insert_pos(body: &[Stmt]) -> usize {
    let mut pos = 0;

    // Skip optional module docstring (a bare string-literal expression).
    if let Some(Stmt::Expr(e)) = body.first() {
        if matches!(&*e.value, Expr::StringLiteral(_)) {
            pos = 1;
        }
    }

    // Skip `from __future__ import ...` statements.
    while pos < body.len() {
        if let Stmt::ImportFrom(imp) = &body[pos] {
            if imp.module.as_ref().map(|m| m.as_str()) == Some("__future__") {
                pos += 1;
                continue;
            }
        }
        break;
    }

    pos
}

// ── recursive statement desugaring ──────────────────────────────────────────

/// Bundle of class-line-offset slices threaded through the recursive desugar
/// walk so individual classes can be checked against multiple modifier flags
/// (`class!` raw, `class … frozen`) without ballooning the function
/// signature.
#[derive(Copy, Clone)]
struct ClassMarkers<'a> {
    raw_starts: &'a [u32],
    frozen_starts: &'a [u32],
    plain_starts: &'a [u32],
    /// Names of classes that are referenced as a base in a multi-inheritance
    /// (`class C(A, B):`) site somewhere in the same module. Adding
    /// `slots=True` to either of A or B would later cause C's class
    /// definition to raise `TypeError: multiple bases have instance
    /// lay-out conflict`, so those classes are emitted without
    /// `slots=True`. FINDINGS #102.
    multi_base_parents: &'a std::collections::HashSet<&'a str>,
    /// User-supplied list of base names whose subclasses should skip the
    /// auto `@dataclass` decoration. Matched by last identifier segment
    /// against each base in the class header.
    skip_decoration_bases: &'a [String],
    /// Value for the `extra` argument in the synthesised
    /// `model_config = ConfigDict(extra=…)` statement for Pydantic `model`
    /// classes. Sourced from `[emit] model-extra` in `typhon.toml` via
    /// [`DesugarOptions::model_extra`].
    model_extra: &'a str,
    /// Names of every module-level class that is (transitively) an exception
    /// subclass — so a subclass of a non-suffix-named user exception base
    /// (`class Timeout(Failure)` where `Failure(Exception)`) is recognised.
    exception_class_names: &'a std::collections::HashSet<String>,
    /// Names of every module-level class. Used to tell an *external*
    /// (builtin/imported) exception base apart from a `*Error`-named module
    /// dataclass when classifying a class as an exception per-class.
    module_class_names: &'a std::collections::HashSet<String>,
}

impl ClassMarkers<'_> {
    /// Return `true` if `starts` contains an offset in the half-open
    /// range `[class_start, name_start)` — i.e. a marker that lives on
    /// the same line as this class's `class` keyword.
    fn marker_covers(starts: &[u32], class_start: u32, name_start: u32) -> bool {
        starts.partition_point(|&off| off < class_start)
            != starts.partition_point(|&off| off < name_start)
    }
    fn is_raw(self, class_start: u32, name_start: u32) -> bool {
        Self::marker_covers(self.raw_starts, class_start, name_start)
    }
    fn is_frozen(self, class_start: u32, name_start: u32) -> bool {
        Self::marker_covers(self.frozen_starts, class_start, name_start)
    }
    fn is_plain(self, class_start: u32, name_start: u32) -> bool {
        Self::marker_covers(self.plain_starts, class_start, name_start)
    }
}

/// Desugar a list of statements, returning the transformed list and whether
/// any class was modified at any nesting depth.
fn desugar_stmts(stmts: &[Stmt], markers: ClassMarkers<'_>) -> (Vec<Stmt>, bool) {
    let mut any_transformed = false;
    let new_stmts = stmts
        .iter()
        .map(|stmt| {
            let (new_stmt, transformed) = desugar_stmt(stmt, markers);
            if transformed {
                any_transformed = true;
            }
            new_stmt
        })
        .collect();
    (new_stmts, any_transformed)
}

/// Desugar a single statement, recursing into any nested statement lists.
/// Returns the (possibly modified) statement and whether any class was
/// transformed at this level or deeper.  `markers` carries the per-class
/// modifier offsets collected by the preprocessor (`class!` raw,
/// `frozen`); classes whose source range covers a marker are emitted
/// with the corresponding decorator (or no decorator, for `class!`).
fn desugar_stmt(stmt: &Stmt, markers: ClassMarkers<'_>) -> (Stmt, bool) {
    match stmt {
        Stmt::ClassDef(c) => {
            // We can't compare against `c.range.start()` directly because
            // Ruff sets that to the `@` token when the class has
            // decorators — but the preprocessor's recorded offset points
            // at the `class` keyword, which sits on a later line. Match
            // instead by looking for any recorded offset in the half-open
            // range `[c.range.start(), c.name.range.start())` — the
            // `class` keyword always lives in that window regardless of
            // decorators, and a nested raw class inside this one would
            // sit beyond `c.name.range.start()`.
            let class_start = u32::from(c.range.start());
            let name_start = u32::from(c.name.range.start());
            let is_raw = markers.is_raw(class_start, name_start);
            let is_frozen = markers.is_frozen(class_start, name_start);
            let is_plain = markers.is_plain(class_start, name_start);
            let is_pydantic = class_inherits_basemodel(c);
            // `impl` pseudo-classes (`__typhon_impl_*`) are temporary stubs
            // that will be merged into their target class by `merge_impl_blocks`;
            // they must not receive a dataclass decorator.
            let is_impl_stub = c.name.as_str().starts_with("__typhon_impl_");
            // `lazy import` lowers to a `__TyphonLazy_*` proxy class with its
            // own `__slots__` and `__init__`; decorating it as a dataclass
            // would rewrite those and break the proxy.
            let is_lazy_proxy = c.name.as_str().starts_with("__TyphonLazy_");
            // `interface` lowers to `class X(Protocol):` — Protocols are not
            // dataclasses (the runtime Protocol behaviour conflicts with
            // dataclass field collection).
            let is_protocol = class_inherits_protocol(c);
            let is_typed_dict = class_inherits_typed_dict(c);
            let is_named_tuple = class_inherits_named_tuple(c);
            // Skip auto-decoration for subclasses of non-dataclass-friendly
            // bases: stdlib `Enum`/`Flag`/`ABC` and any user-supplied names
            // from `skip_decoration_bases`. This makes plain `class X(Enum):`
            // do the right thing without requiring `plain class`/`class!`.
            let is_skip_decoration_subclass =
                class_inherits_skip_decoration_base(c, markers.skip_decoration_bases);
            // Exception subclasses must not get a dataclass `__init__` (it
            // would shadow `BaseException.__init__` and break
            // `raise FooError("msg")`). They lower like `class!`: no
            // decorator, plus a `super().__init__(...)` constructor when the
            // body has fields. A class is an exception when it has an
            // *external* (builtin/imported) exception base — works in any
            // scope including nested classes — OR is a module-level class
            // transitively rooted in one. A `*Error`-named module *dataclass*
            // base is NOT external, so it doesn't taint its subclasses.
            let has_external_exception_base = c.bases().iter().any(|b| {
                base_last_segment(b).is_some_and(|seg| {
                    !markers.module_class_names.contains(seg) && name_is_exception_base(seg)
                })
            });
            let is_exception_subclass = has_external_exception_base
                || markers.exception_class_names.contains(c.name.as_str());
            // Multi-inheritance with concrete bases conflicts with
            // `slots=True`; emit the decorator without `slots=True` in
            // that case. FINDINGS #102. Also drop `slots=True` for any
            // class that is itself a base of a multi-inheritance child
            // class — both bases need to be unslotted for the child to
            // load.
            let has_multi_bases = class_has_multiple_concrete_bases(c)
                || markers.multi_base_parents.contains(c.name.as_str());
            // Skip the dataclass decorator for Pydantic model classes,
            // Protocol classes, TypedDict subclasses, NamedTuple subclasses,
            // and lazy proxies; they already carry the right shape or are
            // incompatible with dataclass.
            let needs_decorator = !is_raw
                && !is_plain
                && !is_pydantic
                && !is_protocol
                && !is_typed_dict
                && !is_named_tuple
                && !is_impl_stub
                && !is_lazy_proxy
                && !is_skip_decoration_subclass
                && !is_exception_subclass
                && !has_dataclass_decorator(&c.decorator_list);
            // Pydantic `model` classes must have `model_config = ConfigDict(extra="forbid")`
            // as their first body statement unless the user already defined it.
            let needs_model_config =
                is_pydantic && !is_impl_stub && !has_model_config_stmt(&c.body);
            let (mut new_body, mut body_transformed) = desugar_stmts(&c.body, markers);
            // Rewrite mutable defaults so Python's dataclass decorator
            // doesn't raise `ValueError: mutable default ... is not allowed:
            // use default_factory`. Targets `[]`, `{}`, `list()`, `dict()`,
            // `set()` literals on class-body field annotations. We only do
            // this for classes that will receive the dataclass decorator —
            // pydantic models, protocols, and impl stubs keep their bodies
            // untouched. FINDINGS #62.
            if needs_decorator && rewrite_mutable_field_defaults(&mut new_body) {
                body_transformed = true;
            }
            let mut new_class = c.clone();
            new_class.body = new_body;
            if needs_decorator {
                let decorator = if is_frozen {
                    make_dataclasses_dot_dataclass_decorator_frozen()
                } else if has_multi_bases {
                    make_dataclasses_dot_dataclass_decorator_no_slots()
                } else {
                    make_dataclasses_dot_dataclass_decorator()
                };
                new_class.decorator_list.insert(0, decorator);
            }
            if needs_model_config {
                // Insert after a leading class docstring so that `__doc__` is
                // preserved.  Python requires the docstring to be the first
                // statement in the class body.
                let insert_at = if let Some(Stmt::Expr(e)) = new_class.body.first() {
                    if matches!(&*e.value, Expr::StringLiteral(_)) {
                        1
                    } else {
                        0
                    }
                } else {
                    0
                };
                new_class
                    .body
                    .insert(insert_at, make_model_config_stmt(markers.model_extra));
            }
            // `class!` classes whose body lacks an explicit `__init__` AND
            // which have at least one positional base get a synthesised
            // constructor: `super().__init__()` followed by `self.x = x` for
            // every annotated field, in source order. The class-level field
            // annotations are kept so type checkers still see the field
            // shape, but their default expressions are stripped — the
            // synthesised `__init__` carries the defaults in its parameter
            // list, so leaving them at class scope would evaluate every
            // default twice (once at class-definition time as a shared
            // class attribute, then again per-instance in `__init__`).
            // That double-eval is harmless for cheap literals like `0.5`
            // but allocates real objects for things like `Linear(10, 5)`
            // and confuses libraries that introspect class attributes
            // (e.g. PyTorch parameter registration on subclasses).
            // `class!` always gets a synthesised constructor (the framework
            // base may need `super().__init__()` called even with no fields).
            // An auto-detected exception subclass only needs one when it
            // declares fields — otherwise it stays bare and inherits
            // `BaseException.__init__`.
            let wants_raw_init =
                is_raw || (is_exception_subclass && class_has_annotated_fields(&new_class.body));
            if wants_raw_init
                && !is_plain
                && class_has_any_base(c)
                && !body_has_init(&new_class.body)
                && !class_has_enum_base(c)
            {
                let synthesised = synthesise_raw_class_init(&new_class.body);
                // Place `__init__` after the leading run of (docstring +
                // field annotations) so the class reads top-to-bottom:
                // doc → fields → __init__ → methods.
                let insert_at = raw_class_init_insert_pos(&new_class.body);
                new_class.body.insert(insert_at, synthesised);
                strip_field_defaults(&mut new_class.body);
            }
            // Only propagate `true` when a dataclass decorator was added (or
            // was added deeper in the body) — that's the signal used by
            // `desugar_mod_module` to decide whether to inject `import dataclasses`.
            // `needs_model_config` is a pydantic-only change and must not trigger
            // the dataclasses import.
            (
                Stmt::ClassDef(new_class),
                needs_decorator || body_transformed,
            )
        }
        Stmt::FunctionDef(f) => {
            let (new_body, transformed) = desugar_stmts(&f.body, markers);
            let mut new_f = f.clone();
            new_f.body = new_body;
            (Stmt::FunctionDef(new_f), transformed)
        }
        other => (other.clone(), false),
    }
}

// ── AST helpers ──────────────────────────────────────────────────────────────

/// Build the decorator `@dataclasses.dataclass(slots=True, frozen=True)`
/// for a `class NAME frozen:` declaration. Companion to
/// [`make_dataclasses_dot_dataclass_decorator`].
fn make_dataclasses_dot_dataclass_decorator_frozen() -> Decorator {
    let dataclasses_name = Expr::Name(ExprName {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        id: Name::new("dataclasses"),
        ctx: ExprContext::Load,
    });
    let dataclass_attr = Expr::Attribute(ExprAttribute {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        value: Box::new(dataclasses_name),
        attr: make_identifier("dataclass"),
        ctx: ExprContext::Load,
    });
    let true_lit_a = Expr::BooleanLiteral(ExprBooleanLiteral {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        value: true,
    });
    let true_lit_b = Expr::BooleanLiteral(ExprBooleanLiteral {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        value: true,
    });
    let call = Expr::Call(ExprCall {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        func: Box::new(dataclass_attr),
        arguments: Arguments {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            args: Box::new([]),
            keywords: Box::new([
                Keyword {
                    range: TextRange::default(),
                    node_index: AtomicNodeIndex::NONE,
                    arg: Some(make_identifier("slots")),
                    value: true_lit_a,
                },
                Keyword {
                    range: TextRange::default(),
                    node_index: AtomicNodeIndex::NONE,
                    arg: Some(make_identifier("frozen")),
                    value: true_lit_b,
                },
            ]),
        },
    });
    Decorator {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        expression: call,
    }
}

/// Walk a class body and rewrite annotated assigns whose value is a
/// mutable literal / no-arg constructor (`[]`, `{}`, `list()`, `dict()`,
/// `set()`) to `dataclasses.field(default_factory=<ctor>)`. Returns
/// `true` if any field was rewritten so the caller can mark the body
/// as transformed (and therefore the `import dataclasses` injection is
/// triggered). FINDINGS #62.
fn rewrite_mutable_field_defaults(body: &mut [Stmt]) -> bool {
    let mut changed = false;
    for stmt in body.iter_mut() {
        let Stmt::AnnAssign(a) = stmt else { continue };
        let Some(value) = &a.value else { continue };
        let Some(factory_name) = mutable_default_factory(value) else {
            continue;
        };
        a.value = Some(Box::new(make_dataclasses_field_default_factory(
            factory_name,
        )));
        changed = true;
    }
    changed
}

/// If `value` is one of the recognised mutable-default expressions,
/// return the name of the built-in factory to thread into
/// `dataclasses.field(default_factory=...)`.
fn mutable_default_factory(value: &Expr) -> Option<&'static str> {
    match value {
        Expr::List(l) if l.elts.is_empty() => Some("list"),
        Expr::Dict(d) if d.items.is_empty() => Some("dict"),
        Expr::Set(s) if s.elts.is_empty() => Some("set"),
        Expr::Call(call)
            if call.arguments.args.is_empty() && call.arguments.keywords.is_empty() =>
        {
            if let Expr::Name(n) = call.func.as_ref() {
                match n.id.as_str() {
                    "list" => Some("list"),
                    "dict" => Some("dict"),
                    "set" => Some("set"),
                    _ => None,
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Build `dataclasses.field(default_factory=<factory_name>)`.
fn make_dataclasses_field_default_factory(factory_name: &str) -> Expr {
    let dataclasses_name = Expr::Name(ExprName {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        id: Name::new("dataclasses"),
        ctx: ExprContext::Load,
    });
    let field_attr = Expr::Attribute(ExprAttribute {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        value: Box::new(dataclasses_name),
        attr: make_identifier("field"),
        ctx: ExprContext::Load,
    });
    let factory_ref = Expr::Name(ExprName {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        id: Name::new(factory_name),
        ctx: ExprContext::Load,
    });
    Expr::Call(ExprCall {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        func: Box::new(field_attr),
        arguments: Arguments {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            args: Box::new([]),
            keywords: Box::new([Keyword {
                range: TextRange::default(),
                node_index: AtomicNodeIndex::NONE,
                arg: Some(make_identifier("default_factory")),
                value: factory_ref,
            }]),
        },
    })
}

/// Build the decorator `@dataclasses.dataclass()` with no keyword
/// arguments (no `slots=True`). Used when the class inherits from
/// other classes whose layout would conflict with slots — Python
/// raises `TypeError: multiple bases have instance lay-out conflict`
/// when two slotted classes are combined. FINDINGS #102.
fn make_dataclasses_dot_dataclass_decorator_no_slots() -> Decorator {
    let dataclasses_name = Expr::Name(ExprName {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        id: Name::new("dataclasses"),
        ctx: ExprContext::Load,
    });

    let dataclass_attr = Expr::Attribute(ExprAttribute {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        value: Box::new(dataclasses_name),
        attr: make_identifier("dataclass"),
        ctx: ExprContext::Load,
    });

    let call = Expr::Call(ExprCall {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        func: Box::new(dataclass_attr),
        arguments: Arguments {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            args: Box::new([]),
            keywords: Box::new([]),
        },
    });

    Decorator {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        expression: call,
    }
}

/// Build the decorator `@dataclasses.dataclass(slots=True)`.
///
/// Using the qualified form avoids shadowing: even if the user has a local
/// binding named `dataclass`, `dataclasses.dataclass` still resolves to the
/// standard-library function.
fn make_dataclasses_dot_dataclass_decorator() -> Decorator {
    let dataclasses_name = Expr::Name(ExprName {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        id: Name::new("dataclasses"),
        ctx: ExprContext::Load,
    });

    let dataclass_attr = Expr::Attribute(ExprAttribute {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        value: Box::new(dataclasses_name),
        attr: make_identifier("dataclass"),
        ctx: ExprContext::Load,
    });

    let true_lit = Expr::BooleanLiteral(ExprBooleanLiteral {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        value: true,
    });

    let call = Expr::Call(ExprCall {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        func: Box::new(dataclass_attr),
        arguments: Arguments {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            args: Box::new([]),
            keywords: Box::new([Keyword {
                range: TextRange::default(),
                node_index: AtomicNodeIndex::NONE,
                arg: Some(make_identifier("slots")),
                value: true_lit,
            }]),
        },
    });

    Decorator {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        expression: call,
    }
}

/// Build the statement `import dataclasses`.
fn make_dataclasses_import() -> Stmt {
    Stmt::Import(StmtImport {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        names: vec![make_alias("dataclasses")],
        is_lazy: false,
    })
}

/// Return `true` if any statement in `stmts` (recursively) uses `BaseModel`
/// as a base class — i.e. the module was produced from `model` keywords.
fn stmts_use_basemodel(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|stmt| match stmt {
        Stmt::ClassDef(c) => class_inherits_basemodel(c) || stmts_use_basemodel(&c.body),
        Stmt::FunctionDef(f) => stmts_use_basemodel(&f.body),
        Stmt::If(s) => {
            stmts_use_basemodel(&s.body)
                || s.elif_else_clauses
                    .iter()
                    .any(|c| stmts_use_basemodel(&c.body))
        }
        Stmt::For(s) => stmts_use_basemodel(&s.body) || stmts_use_basemodel(&s.orelse),
        Stmt::While(s) => stmts_use_basemodel(&s.body) || stmts_use_basemodel(&s.orelse),
        Stmt::With(s) => stmts_use_basemodel(&s.body),
        Stmt::Try(s) => {
            stmts_use_basemodel(&s.body)
                || s.handlers.iter().any(|h| match h {
                    ExceptHandler::ExceptHandler(eh) => stmts_use_basemodel(&eh.body),
                })
                || stmts_use_basemodel(&s.orelse)
                || stmts_use_basemodel(&s.finalbody)
        }
        _ => false,
    })
}

/// Return `true` if `c` inherits directly from `BaseModel`.
fn class_inherits_basemodel(c: &ruff_python_ast::StmtClassDef) -> bool {
    c.bases()
        .iter()
        .any(|base| matches!(base, Expr::Name(n) if n.id.as_str() == "BaseModel"))
}

/// Return `true` if `c` inherits directly from `Protocol`.
fn class_inherits_protocol(c: &ruff_python_ast::StmtClassDef) -> bool {
    c.bases()
        .iter()
        .any(|base| matches!(base, Expr::Name(n) if n.id.as_str() == "Protocol"))
}

/// Pre-scan the module body and collect the names of every class that is
/// referenced as a base in a multi-inheritance site (`class C(A, B):`).
/// Those classes can't carry `slots=True` because two slotted bases
/// trigger `TypeError: multiple bases have instance lay-out conflict`
/// at class-definition time. FINDINGS #102.
/// Walk the module body looking for `__typhon_impl_X` stubs that contain a
/// method decorated with `cached_property` (or its aliased import name) or a
/// plain class with such a method, and record the underlying class names.
/// `cached_property` requires `__dict__`, which conflicts with `slots=True`.
fn collect_cached_property_targets_into<'a>(
    body: &'a [Stmt],
    parents: &mut std::collections::HashSet<&'a str>,
) {
    for stmt in body {
        if let Stmt::ClassDef(c) = stmt {
            let target = c
                .name
                .as_str()
                .strip_prefix("__typhon_impl_")
                .unwrap_or(c.name.as_str());
            if class_body_has_cached_property(&c.body) {
                parents.insert(target);
            }
        }
    }
}

fn class_body_has_cached_property(body: &[Stmt]) -> bool {
    for stmt in body {
        if let Stmt::FunctionDef(f) = stmt {
            for d in &f.decorator_list {
                let name = match &d.expression {
                    Expr::Name(n) => n.id.as_str(),
                    Expr::Attribute(a) => a.attr.as_str(),
                    Expr::Call(c) => match c.func.as_ref() {
                        Expr::Name(n) => n.id.as_str(),
                        Expr::Attribute(a) => a.attr.as_str(),
                        _ => continue,
                    },
                    _ => continue,
                };
                if matches!(name, "cached_property" | "_typhon_cached_property") {
                    return true;
                }
            }
        }
    }
    false
}

fn collect_multi_base_parents<'a>(body: &'a [Stmt]) -> std::collections::HashSet<&'a str> {
    let mut parents: std::collections::HashSet<&'a str> = std::collections::HashSet::new();
    collect_multi_base_parents_into(body, &mut parents);
    parents
}

fn collect_multi_base_parents_into<'a>(
    body: &'a [Stmt],
    parents: &mut std::collections::HashSet<&'a str>,
) {
    for stmt in body {
        match stmt {
            Stmt::ClassDef(c) => {
                let concrete_bases: Vec<&Expr> = c
                    .bases()
                    .iter()
                    .filter(|b| !is_layout_neutral_base(b))
                    .collect();
                if concrete_bases.len() > 1 {
                    for b in &concrete_bases {
                        if let Expr::Name(n) = b {
                            parents.insert(n.id.as_str());
                        }
                    }
                }
                collect_multi_base_parents_into(&c.body, parents);
            }
            Stmt::FunctionDef(f) => {
                // Ruff folds `async def` into the same `FunctionDef`
                // node (with `is_async = true`), so this arm covers
                // both sync and async function bodies.
                collect_multi_base_parents_into(&f.body, parents);
            }
            Stmt::If(i) => {
                collect_multi_base_parents_into(&i.body, parents);
                for clause in &i.elif_else_clauses {
                    collect_multi_base_parents_into(&clause.body, parents);
                }
            }
            Stmt::For(f) => {
                // Ruff also folds `async for` into `For` (`is_async`).
                collect_multi_base_parents_into(&f.body, parents);
                collect_multi_base_parents_into(&f.orelse, parents);
            }
            Stmt::While(w) => {
                collect_multi_base_parents_into(&w.body, parents);
                collect_multi_base_parents_into(&w.orelse, parents);
            }
            Stmt::With(w) => {
                // Ruff folds `async with` into `With` (`is_async`).
                collect_multi_base_parents_into(&w.body, parents);
            }
            Stmt::Match(m) => {
                for case in m.cases.iter() {
                    collect_multi_base_parents_into(&case.body, parents);
                }
            }
            Stmt::Try(t) => {
                collect_multi_base_parents_into(&t.body, parents);
                for handler in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = handler;
                    collect_multi_base_parents_into(&h.body, parents);
                }
                collect_multi_base_parents_into(&t.orelse, parents);
                collect_multi_base_parents_into(&t.finalbody, parents);
            }
            _ => {}
        }
    }
}

/// Return `true` if `c` inherits directly from `NamedTuple`.
///
/// `NamedTuple` subclasses must not receive `@dataclasses.dataclass(slots=True)`
/// — Python's `NamedTuple` metaclass already defines `__slots__` and adding
/// the dataclass decorator triggers `TypeError: Point already specifies
/// __slots__`. FINDINGS #101.
///
/// Handles bare-name (`NamedTuple`), `typing.NamedTuple`, and
/// `typing_extensions.NamedTuple` forms.
fn class_inherits_named_tuple(c: &ruff_python_ast::StmtClassDef) -> bool {
    c.bases().iter().any(|base| match base {
        Expr::Name(n) => n.id.as_str() == "NamedTuple",
        Expr::Attribute(a) => {
            a.attr.as_str() == "NamedTuple"
                && matches!(
                    &*a.value,
                    Expr::Name(n)
                    if n.id.as_str() == "typing" || n.id.as_str() == "typing_extensions"
                )
        }
        _ => false,
    })
}

/// Return `true` if `c` declares more than one concrete (non-`Protocol`,
/// non-`Generic[T]`) base class. Adding `@dataclasses.dataclass(slots=True)`
/// to a class with multiple slotted bases raises `TypeError: multiple
/// bases have instance lay-out conflict` at class-definition time, so the
/// decorator must be applied without `slots=True` in this case.
/// FINDINGS #102.
fn class_has_multiple_concrete_bases(c: &ruff_python_ast::StmtClassDef) -> bool {
    let count = c
        .bases()
        .iter()
        .filter(|base| !is_layout_neutral_base(base))
        .count();
    count > 1
}

/// `Protocol`, `Generic[T]`, and `object` don't carry an instance layout
/// of their own, so they don't contribute to a multiple-base slot
/// conflict. Everything else (user classes, `BaseException`, etc.) does.
fn is_layout_neutral_base(base: &Expr) -> bool {
    fn last_name(e: &Expr) -> Option<&str> {
        match e {
            Expr::Name(n) => Some(n.id.as_str()),
            Expr::Attribute(a) => Some(a.attr.as_str()),
            Expr::Subscript(s) => last_name(&s.value),
            _ => None,
        }
    }
    matches!(
        last_name(base),
        Some("Protocol") | Some("Generic") | Some("object")
    )
}

/// Stdlib base names whose subclasses should NOT receive the auto
/// `@dataclasses.dataclass(slots=True)` decoration. Adding the dataclass
/// decorator to an `Enum`/`Flag`/`ABC` subclass either silently breaks
/// semantics (enum members get rewritten into instance fields) or raises
/// `TypeError` at class-definition time.
const SKIP_DECORATION_BUILTIN_BASES: &[&str] = &[
    "Enum", "IntEnum", "StrEnum", "Flag", "IntFlag", "ABC", "ABCMeta",
];

/// Walk a base expression and return its trailing identifier segment.
///
/// Handles:
/// - `Expr::Name(n)` → `n.id`
/// - `Expr::Attribute(a)` (e.g. `enum.Enum`) → `a.attr`
/// - `Expr::Subscript(s)` (e.g. `Generic[T]`, `MyBase[int, str]`) →
///   recurses into the value side
///
/// Anything else returns `None` (call expressions in base lists are rare
/// outside of metaclass tricks and don't need to participate in the
/// skip-decoration heuristic).
fn base_last_segment(base: &Expr) -> Option<&str> {
    match base {
        Expr::Name(n) => Some(n.id.as_str()),
        Expr::Attribute(a) => Some(a.attr.as_str()),
        Expr::Subscript(s) => base_last_segment(&s.value),
        _ => None,
    }
}

/// Return `true` if any base of `c` matches a known non-dataclass-friendly
/// stdlib parent (`Enum`/`Flag`/`ABC` family) or a user-supplied entry in
/// `extra`. User-supplied names are matched by last identifier segment so
/// `"App"` covers both `class T(App):` and `class T(textual.App):`.
fn class_inherits_skip_decoration_base(
    c: &ruff_python_ast::StmtClassDef,
    extra: &[String],
) -> bool {
    c.bases().iter().any(|base| {
        let Some(seg) = base_last_segment(base) else {
            return false;
        };
        if SKIP_DECORATION_BUILTIN_BASES.contains(&seg) {
            return true;
        }
        extra.iter().any(|name| {
            // Allow either a bare last-segment name or a dotted path
            // (last segment of the configured name is compared).
            let configured_seg = name.rsplit('.').next().unwrap_or(name.as_str());
            configured_seg == seg
        })
    })
}

/// Return `true` if `c` inherits directly from `TypedDict`.
///
/// `TypedDict` subclasses must not receive `@dataclasses.dataclass(slots=True)`
/// — Python raises `TypeError: cannot inherit from both a TypedDict type and a
/// non-TypedDict base class` at class-definition time. FINDINGS #67.
///
/// Handles bare-name (`TypedDict`), `typing.TypedDict`, and
/// `typing_extensions.TypedDict` forms.
fn class_inherits_typed_dict(c: &ruff_python_ast::StmtClassDef) -> bool {
    c.bases().iter().any(|base| match base {
        // `class X(TypedDict):`
        Expr::Name(n) => n.id.as_str() == "TypedDict",
        // `class X(typing.TypedDict):` or `class X(typing_extensions.TypedDict):`
        Expr::Attribute(a) => {
            a.attr.as_str() == "TypedDict"
                && matches!(
                    &*a.value,
                    Expr::Name(n)
                    if n.id.as_str() == "typing" || n.id.as_str() == "typing_extensions"
                )
        }
        _ => false,
    })
}

/// Exact base names — outside the `*Error`/`*Exception`/`*Warning` naming
/// convention — that nonetheless make a class an exception subclass.
const EXACT_EXCEPTION_BASES: &[&str] = &[
    "BaseException",
    "KeyboardInterrupt",
    "SystemExit",
    "GeneratorExit",
    "StopIteration",
    "StopAsyncIteration",
];

/// Whether `name` (a base's trailing segment) marks an exception by the
/// builtin convention: a `*Error` / `*Exception` / `*Warning` suffix or an
/// exact non-suffixed builtin (`BaseException`, `KeyboardInterrupt`, …).
fn name_is_exception_base(name: &str) -> bool {
    name.ends_with("Error")
        || name.ends_with("Exception")
        || name.ends_with("Warning")
        || EXACT_EXCEPTION_BASES.contains(&name)
}

/// Names of every class in the module that is (transitively) an exception
/// subclass. A class qualifies when a base is an *external* (builtin/imported)
/// name matching the exception convention (`Exception`, `ValueError`, …) OR
/// names another module class that itself qualifies (transitively). Crucially,
/// a `*Error`-named base that is itself a *module* class is NOT assumed to be
/// an exception: `class LexError: line: int` is a Result error-variant
/// dataclass, so `class Detailed(LexError):` stays a dataclass too. But
/// `class Failure(Exception): pass` then `class Timeout(Failure): pass` both
/// qualify, since `Failure` is rooted in the builtin `Exception`.
fn exception_class_names_from(
    classes: &[(String, Vec<String>)],
    module_classes: &std::collections::HashSet<String>,
) -> std::collections::HashSet<String> {
    let mut exc: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Seed only from external exception bases — a `*Error`-named *module*
    // class is left to the fixpoint (it qualifies only if rooted in a builtin
    // exception), so a plain `*Error` dataclass base doesn't taint subclasses.
    for (name, bases) in classes {
        if bases
            .iter()
            .any(|b| !module_classes.contains(b.as_str()) && name_is_exception_base(b))
        {
            exc.insert(name.clone());
        }
    }
    loop {
        let mut changed = false;
        for (name, bases) in classes {
            if !exc.contains(name) && bases.iter().any(|b| exc.contains(b)) {
                exc.insert(name.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    exc
}

/// Collect `(class name, base trailing segments)` for every *module-level*
/// class def in `body` — descending through module-level control flow
/// (`if`/`try`/`for`/`while`/`with`) but NOT into function or nested-class
/// bodies. Keeping the set module-scoped avoids a function-local
/// `class Failure(Exception):` tainting an unrelated top-level
/// `class Failure:` dataclass of the same name. Nested-scope exception
/// classes are recognised per-class at decoration time via their *external*
/// base (see `is_exception_subclass` in `desugar_stmts`).
fn collect_class_bases_into(body: &[Stmt], out: &mut Vec<(String, Vec<String>)>) {
    for stmt in body {
        match stmt {
            Stmt::ClassDef(c) => {
                let bases: Vec<String> = c
                    .bases()
                    .iter()
                    .filter_map(|b| base_last_segment(b).map(|s| s.to_owned()))
                    .collect();
                out.push((c.name.as_str().to_owned(), bases));
                // Do NOT descend into the class body — a class nested inside a
                // class is a different scope.
            }
            // Do NOT descend into function bodies (a different scope).
            Stmt::If(i) => {
                collect_class_bases_into(&i.body, out);
                for clause in &i.elif_else_clauses {
                    collect_class_bases_into(&clause.body, out);
                }
            }
            Stmt::For(f) => {
                collect_class_bases_into(&f.body, out);
                collect_class_bases_into(&f.orelse, out);
            }
            Stmt::While(w) => {
                collect_class_bases_into(&w.body, out);
                collect_class_bases_into(&w.orelse, out);
            }
            Stmt::With(w) => collect_class_bases_into(&w.body, out),
            Stmt::Try(t) => {
                collect_class_bases_into(&t.body, out);
                for h in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    collect_class_bases_into(&h.body, out);
                }
                collect_class_bases_into(&t.orelse, out);
                collect_class_bases_into(&t.finalbody, out);
            }
            _ => {}
        }
    }
}

/// Return `true` if the class body declares at least one top-level annotated
/// field (`name: T` / `name: T = default`). Used to decide whether an
/// exception subclass needs a synthesised field-assigning `__init__`: a
/// field-less `class FooError(Exception): pass` should stay bare and inherit
/// `BaseException.__init__` (so `raise FooError("msg")` just works) rather
/// than carry a redundant `*args`/`**kwargs` passthrough.
fn class_has_annotated_fields(body: &[Stmt]) -> bool {
    body.iter().any(|s| {
        matches!(
            s,
            Stmt::AnnAssign(a) if matches!(a.target.as_ref(), Expr::Name(_))
        )
    })
}

/// Return `true` if the module already has `from pydantic import BaseModel`
/// where the name is bound as `BaseModel` (not aliased to something else).
fn has_pydantic_import(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::ImportFrom(imp) => {
            imp.module.as_ref().map(|m| m.as_str()) == Some("pydantic")
                && imp.names.iter().any(|a| {
                    a.name.as_str() == "BaseModel"
                        // Only suppress injection when the bound name is
                        // actually `BaseModel`, not an alias like `BM`.
                        && matches!(
                            a.asname.as_ref().map(|n| n.as_str()),
                            None | Some("BaseModel")
                        )
                })
        }
        _ => false,
    })
}

/// Build `from pydantic import BaseModel, ConfigDict`.
fn make_pydantic_basemodel_import() -> Stmt {
    Stmt::ImportFrom(StmtImportFrom {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        module: Some(make_identifier("pydantic")),
        names: vec![make_alias("BaseModel"), make_alias("ConfigDict")],
        level: 0,
        is_lazy: false,
    })
}

/// Build `from pydantic import BaseModel` (without ConfigDict).
///
/// Used when only BaseModel is missing — ConfigDict is already imported.
fn make_pydantic_basemodel_only_import() -> Stmt {
    Stmt::ImportFrom(StmtImportFrom {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        module: Some(make_identifier("pydantic")),
        names: vec![make_alias("BaseModel")],
        level: 0,
        is_lazy: false,
    })
}

/// Build `from pydantic import ConfigDict` (without BaseModel).
///
/// Used when a module already imports `BaseModel` explicitly but does not yet
/// import `ConfigDict`, which is needed for the injected `model_config` statement.
fn make_config_dict_only_import() -> Stmt {
    Stmt::ImportFrom(StmtImportFrom {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        module: Some(make_identifier("pydantic")),
        names: vec![make_alias("ConfigDict")],
        level: 0,
        is_lazy: false,
    })
}

/// Return `true` if `body` already imports `ConfigDict` from `pydantic`.
fn has_config_dict_import(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::ImportFrom(imp) => {
            imp.module.as_ref().map(|m| m.as_str()) == Some("pydantic")
                && imp
                    .names
                    .iter()
                    .any(|a| matches!(a.name.as_str(), "ConfigDict" | "*"))
        }
        _ => false,
    })
}

/// Return `true` if `body` already contains a `model_config = ...` statement.
fn has_model_config_stmt(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::Assign(a) => a
            .targets
            .iter()
            .any(|t| matches!(t, Expr::Name(n) if n.id.as_str() == "model_config")),
        Stmt::AnnAssign(a) => {
            matches!(a.target.as_ref(), Expr::Name(n) if n.id.as_str() == "model_config")
        }
        _ => false,
    })
}

/// Build `model_config = ConfigDict(extra="…")`.
///
/// The `extra` argument is the string literal value passed to
/// `ConfigDict(extra=…)` — one of `"forbid"`, `"ignore"`, or `"allow"`.
/// Any validated value from `[emit] model-extra` in `typhon.toml` is
/// accepted; callers are responsible for passing a valid value.
fn make_model_config_stmt(extra: &str) -> Stmt {
    let forbid_lit = make_string_literal_expr(extra);

    let config_dict_call = Expr::Call(ExprCall {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        func: Box::new(Expr::Name(ExprName {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            id: Name::new("ConfigDict"),
            ctx: ExprContext::Load,
        })),
        arguments: Arguments {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            args: Box::new([]),
            keywords: Box::new([Keyword {
                range: TextRange::default(),
                node_index: AtomicNodeIndex::NONE,
                arg: Some(make_identifier("extra")),
                value: forbid_lit,
            }]),
        },
    });

    Stmt::Assign(StmtAssign {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        targets: vec![Expr::Name(ExprName {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            id: Name::new("model_config"),
            ctx: ExprContext::Store,
        })],
        value: Box::new(config_dict_call),
        mutability: None,
    })
}

/// Return `true` if the decorator list already contains any recognized form of
/// the dataclass decorator:
/// - `@dataclass`          (bare name, from-import style)
/// - `@dataclass(...)`     (call, from-import style)
/// - `@dataclasses.dataclass`
/// - `@dataclasses.dataclass(...)`
fn has_dataclass_decorator(decorators: &[Decorator]) -> bool {
    decorators.iter().any(|d| is_dataclass_expr(&d.expression))
}

fn is_dataclass_expr(expr: &Expr) -> bool {
    match expr {
        // @dataclass
        Expr::Name(n) => n.id.as_str() == "dataclass",
        // @dataclasses.dataclass
        Expr::Attribute(a) => {
            a.attr.as_str() == "dataclass"
                && matches!(a.value.as_ref(),
                    Expr::Name(n) if n.id.as_str() == "dataclasses"
                )
        }
        // @dataclass(...) or @dataclasses.dataclass(...)
        Expr::Call(c) => is_dataclass_expr(c.func.as_ref()),
        _ => false,
    }
}

/// Return `true` if the body already contains `import dataclasses` or
/// `from dataclasses import dataclass` (either form means the import is covered).
fn has_dataclasses_import(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::Import(imp) => imp.names.iter().any(|a| a.name.as_str() == "dataclasses"),
        Stmt::ImportFrom(imp) => {
            imp.module.as_ref().map(|m| m.as_str()) == Some("dataclasses")
                && imp.names.iter().any(|a| a.name.as_str() == "dataclass")
        }
        _ => false,
    })
}

// ── small construction helpers ──────────────────────────────────────────────

fn make_identifier(name: &str) -> Identifier {
    Identifier::new(name, TextRange::default())
}

fn make_alias(name: &str) -> Alias {
    Alias {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        name: make_identifier(name),
        asname: None,
    }
}

// ── `class!` __init__ synthesis ─────────────────────────────────────────────

/// True when the class has at least one positional base (`class Foo(Bar):`).
/// Keyword arguments alone — `metaclass=`, `total=False`, etc. — don't
/// count; a class with no positional base has nothing meaningful to chain
/// `super().__init__()` through.
fn class_has_any_base(c: &ruff_python_ast::StmtClassDef) -> bool {
    c.arguments
        .as_ref()
        .map(|a| !a.args.is_empty())
        .unwrap_or(false)
}

/// True when the class body already defines a `def __init__`.  We never
/// overwrite an author-written constructor.
fn body_has_init(body: &[Stmt]) -> bool {
    body.iter().any(|s| match s {
        Stmt::FunctionDef(f) => f.name.as_str() == "__init__",
        _ => false,
    })
}

/// Insertion index for a synthesised `__init__`: skip past a leading
/// docstring and the contiguous run of field annotations that follows.
/// Stops at the first method (FunctionDef), assignment without
/// annotation, or other statement so the synthesised constructor lands
/// where a hand-written one would naturally go.
fn raw_class_init_insert_pos(body: &[Stmt]) -> usize {
    let mut idx = 0;
    if let Some(Stmt::Expr(e)) = body.first() {
        if matches!(&*e.value, Expr::StringLiteral(_)) {
            idx = 1;
        }
    }
    while let Some(stmt) = body.get(idx) {
        if matches!(stmt, Stmt::AnnAssign(_)) {
            idx += 1;
        } else {
            break;
        }
    }
    idx
}

/// True when `ann` is `ClassVar[...]`, in any spelling the language accepts —
/// bare, `typing.ClassVar`, or an aliased module (`t.ClassVar`).
fn is_classvar_annotation(ann: &Expr) -> bool {
    let head = match ann {
        Expr::Subscript(s) => s.value.as_ref(),
        other => other,
    };
    match head {
        Expr::Name(n) => n.id.as_str() == "ClassVar",
        Expr::Attribute(a) => a.attr.as_str() == "ClassVar",
        _ => false,
    }
}

/// Strip the default value from each top-level `AnnAssign` whose target
/// is a plain `Name`. Used after `synthesise_raw_class_init` has folded
/// those defaults into the generated `__init__` signature — keeping them
/// at class scope as well would mean the default expression is
/// evaluated twice (once as a class attribute at class-definition time,
/// then again per-instance in `__init__`).
///
/// Only top-level statements are touched; nested classes / functions
/// are left alone. AnnAssigns with non-`Name` targets (subscripts,
/// attribute writes) are also skipped because `synthesise_raw_class_init`
/// never folds them into the constructor in the first place.
fn strip_field_defaults(body: &mut [Stmt]) {
    for stmt in body.iter_mut() {
        if let Stmt::AnnAssign(a) = stmt {
            // A `ClassVar[T] = default` is a class attribute and its default
            // is the attribute's *value*, not a constructor default. Stripping
            // it deleted the attribute outright, so `Cls.NAME` raised
            // AttributeError on a program that checked clean.
            if is_classvar_annotation(&a.annotation) {
                continue;
            }
            if matches!(a.target.as_ref(), Expr::Name(_)) {
                a.value = None;
            }
        }
    }
}

/// Synthesise an `__init__(self, …) -> None` for a `class!` body. The
/// function calls `super().__init__()` first, then assigns every
/// `AnnAssign`-declared field through `self`. Fields without a default
/// come before fields with one — Python disallows a default-bearing
/// positional parameter from preceding a non-default one, so we
/// stable-partition the field list to keep the generated signature
/// valid. Relative order within each group is preserved.
///
/// When the body has *no* annotated fields (e.g. `class! AppError(Exception):
/// pass`), the generated signature is `def __init__(self, *args, **kwargs)
/// -> None: super().__init__(*args, **kwargs)`. This is the conventional
/// shape for `Exception` subclasses — `raise AppError("oops")` must reach
/// `Exception.__init__("oops")` — and works for any other framework base
/// whose constructor accepts positional or keyword arguments.
fn synthesise_raw_class_init(body: &[Stmt]) -> Stmt {
    use ruff_python_ast::{StmtAnnAssign, StmtFunctionDef};
    // Collect (name, annotation, optional default) for each top-level
    // annotated field. Non-Name targets (subscript / attribute annotations)
    // are skipped — they're not fields.
    let raw_fields: Vec<(&Name, Expr, Option<Expr>)> = body
        .iter()
        .filter_map(|s| {
            if let Stmt::AnnAssign(StmtAnnAssign {
                target,
                annotation,
                value,
                ..
            }) = s
            {
                // `ClassVar[T]` is a *class* attribute, never a constructor
                // parameter. Treating it as one produced
                // `def __init__(self, name, REGISTRY: ClassVar[str] = "widgets")`
                // and — because the synthesiser also strips class-level
                // defaults from the body — deleted the class attribute
                // outright, so `Widget.REGISTRY` raised AttributeError on a
                // clean check.
                if is_classvar_annotation(annotation) {
                    return None;
                }
                if let Expr::Name(n) = target.as_ref() {
                    return Some((&n.id, (**annotation).clone(), value.as_deref().cloned()));
                }
            }
            None
        })
        .collect();

    // No annotated fields → emit a `super()`-passthrough so positional and
    // keyword arguments reach the parent constructor unchanged.
    if raw_fields.is_empty() {
        return Stmt::FunctionDef(StmtFunctionDef {
            node_index: AtomicNodeIndex::NONE,
            range: TextRange::default(),
            is_async: false,
            decorator_list: Vec::new(),
            name: make_identifier("__init__"),
            type_params: None,
            parameters: Box::new(Parameters {
                range: TextRange::default(),
                node_index: AtomicNodeIndex::NONE,
                posonlyargs: Vec::new(),
                args: vec![ParameterWithDefault {
                    range: TextRange::default(),
                    node_index: AtomicNodeIndex::NONE,
                    parameter: Parameter {
                        range: TextRange::default(),
                        node_index: AtomicNodeIndex::NONE,
                        name: make_identifier("self"),
                        annotation: None,
                    },
                    default: None,
                }],
                vararg: Some(Box::new(Parameter {
                    range: TextRange::default(),
                    node_index: AtomicNodeIndex::NONE,
                    name: make_identifier("args"),
                    annotation: None,
                })),
                kwonlyargs: Vec::new(),
                kwarg: Some(Box::new(Parameter {
                    range: TextRange::default(),
                    node_index: AtomicNodeIndex::NONE,
                    name: make_identifier("kwargs"),
                    annotation: None,
                })),
            }),
            returns: Some(Box::new(make_none_expr())),
            body: vec![make_super_init_passthrough_stmt()],
        });
    }

    // Stable partition: non-defaulted params first, then defaulted ones.
    let (no_default, with_default): (Vec<_>, Vec<_>) = raw_fields
        .iter()
        .cloned()
        .partition(|(_, _, default)| default.is_none());
    let fields: Vec<(&Name, Expr, Option<Expr>)> =
        no_default.into_iter().chain(with_default).collect();

    // Build the parameter list: `self` + one entry per field.
    let mut args: Vec<ParameterWithDefault> = Vec::with_capacity(fields.len() + 1);
    args.push(ParameterWithDefault {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        parameter: Parameter {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            name: make_identifier("self"),
            annotation: None,
        },
        default: None,
    });
    for (name, annotation, default) in &fields {
        args.push(ParameterWithDefault {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            parameter: Parameter {
                range: TextRange::default(),
                node_index: AtomicNodeIndex::NONE,
                name: make_identifier(name.as_str()),
                annotation: Some(Box::new(annotation.clone())),
            },
            default: default.as_ref().map(|d| Box::new(d.clone())),
        });
    }

    // Build the body: `super().__init__()` followed by `self.x = x` …
    // Assignments use source order (the `raw_fields` order, not the
    // partitioned signature order) so the body reads top-to-bottom like
    // the original class definition.
    let mut new_body: Vec<Stmt> = Vec::with_capacity(raw_fields.len() + 1);
    new_body.push(make_super_init_call_stmt());
    for (name, _, _) in &raw_fields {
        new_body.push(make_self_field_assign_stmt(name.as_str()));
    }

    Stmt::FunctionDef(StmtFunctionDef {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        is_async: false,
        decorator_list: Vec::new(),
        name: make_identifier("__init__"),
        type_params: None,
        parameters: Box::new(Parameters {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            posonlyargs: Vec::new(),
            args,
            vararg: None,
            kwonlyargs: Vec::new(),
            kwarg: None,
        }),
        returns: Some(Box::new(make_none_expr())),
        body: new_body,
    })
}

/// `super().__init__()` as an expression statement.
fn make_super_init_call_stmt() -> Stmt {
    use ruff_python_ast::StmtExpr;
    // super()
    let super_call = Expr::Call(ExprCall {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        func: Box::new(make_bare_name_expr("super")),
        arguments: Arguments {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            args: Box::new([]),
            keywords: Box::new([]),
        },
    });
    // super().__init__
    let super_init = Expr::Attribute(ExprAttribute {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        value: Box::new(super_call),
        attr: make_identifier("__init__"),
        ctx: ExprContext::Load,
    });
    // super().__init__()
    let call = Expr::Call(ExprCall {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        func: Box::new(super_init),
        arguments: Arguments {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            args: Box::new([]),
            keywords: Box::new([]),
        },
    });
    Stmt::Expr(StmtExpr {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        value: Box::new(call),
    })
}

/// `super().__init__(*args, **kwargs)` as an expression statement.
/// Used when a `class!` body has no annotated fields, so positional and
/// keyword arguments must reach the parent constructor unchanged — the
/// shape `Exception("msg")` and similar framework bases rely on.
fn make_super_init_passthrough_stmt() -> Stmt {
    use ruff_python_ast::{ExprStarred, StmtExpr};
    let super_call = Expr::Call(ExprCall {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        func: Box::new(make_bare_name_expr("super")),
        arguments: Arguments {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            args: Box::new([]),
            keywords: Box::new([]),
        },
    });
    let super_init = Expr::Attribute(ExprAttribute {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        value: Box::new(super_call),
        attr: make_identifier("__init__"),
        ctx: ExprContext::Load,
    });
    let starred_args = Expr::Starred(ExprStarred {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        value: Box::new(make_bare_name_expr("args")),
        ctx: ExprContext::Load,
    });
    let kwargs_kw = Keyword {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        arg: None,
        value: make_bare_name_expr("kwargs"),
    };
    let call = Expr::Call(ExprCall {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        func: Box::new(super_init),
        arguments: Arguments {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            args: Box::new([starred_args]),
            keywords: Box::new([kwargs_kw]),
        },
    });
    Stmt::Expr(StmtExpr {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        value: Box::new(call),
    })
}

/// `self.<name> = <name>` as an assignment statement.
fn make_self_field_assign_stmt(field: &str) -> Stmt {
    let target = Expr::Attribute(ExprAttribute {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        value: Box::new(make_bare_name_expr("self")),
        attr: make_identifier(field),
        ctx: ExprContext::Store,
    });
    let value = make_bare_name_expr(field);
    Stmt::Assign(StmtAssign {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        targets: vec![target],
        value: Box::new(value),
        mutability: None,
    })
}

fn make_bare_name_expr(name: &str) -> Expr {
    Expr::Name(ExprName {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        id: Name::new(name),
        ctx: ExprContext::Load,
    })
}

fn make_none_expr() -> Expr {
    use ruff_python_ast::ExprNoneLiteral;
    Expr::NoneLiteral(ExprNoneLiteral {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
    })
}

fn make_string_literal_expr(text: &str) -> Expr {
    let lit = StringLiteral {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        value: Box::from(text),
        flags: StringLiteralFlags::empty(),
    };
    Expr::StringLiteral(ExprStringLiteral {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        value: StringLiteralValue::single(lit),
    })
}

// ── impl block merging ───────────────────────────────────────────────────────

/// Name prefix the preprocessor gives every `impl` pseudo-class.
const IMPL_PREFIX: &str = "__typhon_impl_";

/// Walk `expr` collecting every bare `Name` operand of a flat `A | B | …`
/// union expression. Returns `None` for any other shape so `type X = A`
/// (single name, no union) and `type X = list[A]` (generic) don't get
/// mistaken for sealed unions. Mirrors
/// `tyc_types::extract_sealed_union_variants` so the desugar doesn't need
/// a cross-crate dep. R2-3.
fn collect_union_variant_names(expr: &Expr) -> Option<Vec<&str>> {
    let mut names = Vec::new();
    let mut stack = vec![expr];
    while let Some(current) = stack.pop() {
        match current {
            Expr::Name(n) => names.push(n.id.as_str()),
            // Push `right` first so the stack pops left-to-right: a
            // `BinOp` reads `A | B | C` as
            // `BinOp(BinOp(A, B), C)` — pushing left last makes the
            // visitor descend leftmost first and preserves source
            // order in `names`. Without this the variants end up in
            // reverse order, contradicting the docstring on the
            // caller (`collect_sealed_union_aliases`). PR #129
            // gemini review.
            Expr::BinOp(b) if matches!(b.op, Operator::BitOr) => {
                stack.push(&b.right);
                stack.push(&b.left);
            }
            _ => return None,
        }
    }
    if names.len() >= 2 {
        Some(names)
    } else {
        None
    }
}

/// Collect every sealed-union type alias declared at module scope so
/// `impl Union:` can distribute its methods across every variant.
/// Keyed by the alias name; values are the ordered variant class names
/// as they appear in `type Union = A | B | C`. R2-3.
fn collect_sealed_union_aliases(body: &[Stmt]) -> HashMap<&str, Vec<&str>> {
    let mut out = HashMap::new();
    for stmt in body {
        if let Stmt::TypeAlias(ta) = stmt {
            if let Expr::Name(n) = ta.name.as_ref() {
                if let Some(variants) = collect_union_variant_names(&ta.value) {
                    out.insert(n.id.as_str(), variants);
                }
            }
        }
    }
    out
}

// ── H0: bare `super()` rewrite ─────────────────────────────────────────────

/// Rewrite every bare zero-argument `super()` call appearing inside a method
/// into the explicit two-argument form `super(EnclosingClass, self)`.
///
/// Background: Typhon emits `@dataclasses.dataclass(slots=True)` on every
/// `class`. With `slots=True` the decorator rebuilds the class object, which
/// orphans the `__class__` closure cell a bare `super()` relies on. The result
/// is a runtime `TypeError` for any inheritance + `super()` program. The
/// two-argument form does not depend on `__class__`, so it works under
/// `slots=True`.
///
/// Runs after `merge_impl_blocks`, so methods brought in from `impl`/`extend`
/// blocks (now sitting inside their target class) are covered as well. Only
/// calls of the exact shape `super()` (zero args, zero keywords) are touched;
/// `super(X, y)` calls are left intact.
use ruff_python_ast::visitor::transformer::Transformer as _SuperTransformer;

fn rewrite_bare_super(mut body: Vec<Stmt>) -> Vec<Stmt> {
    for stmt in &mut body {
        rewrite_bare_super_stmt(stmt);
    }
    body
}

/// Visit a statement looking for class definitions. When one is found, every
/// method in its body is scanned for bare `super()` using that class's name and
/// the method's first parameter as the two `super` arguments. Recurses through
/// compound statements so a class nested inside an `if`/`try`/etc. is reached.
fn rewrite_bare_super_stmt(stmt: &mut Stmt) {
    match stmt {
        Stmt::ClassDef(c) => {
            let class_name = c.name.as_str().to_owned();
            for member in &mut c.body {
                // First, recurse: a class nested inside this one (directly or
                // inside a method) gets its own enclosing-class context.
                rewrite_bare_super_stmt(member);
                // Then, if this member is a method, rewrite its bare `super()`.
                if let Stmt::FunctionDef(f) = member {
                    if let Some(self_name) = first_parameter_name(&f.parameters) {
                        let rewriter = SuperRewriter {
                            class_name: class_name.clone(),
                            self_name,
                        };
                        for body_stmt in &mut f.body {
                            rewriter.visit_stmt(body_stmt);
                        }
                    }
                }
            }
        }
        // Descend into compound statements (if / for / while / with / try /
        // match / module-level def bodies) so nested classes are still handled.
        _ => {
            ruff_python_ast::visitor::transformer::walk_stmt(&StmtClassDescender, stmt);
        }
    }
}

/// A no-op `Transformer` whose only job is to drive `walk_stmt`'s recursion into
/// child statements, re-dispatching each through [`rewrite_bare_super_stmt`] so
/// class definitions anywhere in the tree are discovered.
struct StmtClassDescender;

impl ruff_python_ast::visitor::transformer::Transformer for StmtClassDescender {
    fn visit_stmt(&self, stmt: &mut Stmt) {
        rewrite_bare_super_stmt(stmt);
    }
}

/// Rewrites bare `super()` → `super(class_name, self_name)` within a single
/// method body. Stops at nested `def`/`class` boundaries: those establish a
/// fresh `super()` scope, so rewriting them with this method's class/self would
/// be wrong. Nested classes are still discovered (and their own methods
/// rewritten) by re-dispatching through [`rewrite_bare_super_stmt`].
struct SuperRewriter {
    class_name: String,
    self_name: String,
}

impl ruff_python_ast::visitor::transformer::Transformer for SuperRewriter {
    fn visit_stmt(&self, stmt: &mut Stmt) {
        match stmt {
            // A nested function defines its own `super()` scope; don't rewrite
            // bare `super()` inside it with this method's class/self.
            Stmt::FunctionDef(_) => {}
            // A nested class restarts class discovery.
            Stmt::ClassDef(_) => rewrite_bare_super_stmt(stmt),
            _ => ruff_python_ast::visitor::transformer::walk_stmt(self, stmt),
        }
    }

    fn visit_expr(&self, expr: &mut Expr) {
        // Recurse into children first, then rewrite this node if it is `super()`.
        ruff_python_ast::visitor::transformer::walk_expr(self, expr);
        if let Expr::Call(call) = expr {
            let is_bare_super = matches!(call.func.as_ref(), Expr::Name(n) if n.id.as_str() == "super")
                && call.arguments.args.is_empty()
                && call.arguments.keywords.is_empty();
            if is_bare_super {
                call.arguments.args = Box::new([
                    make_bare_name_expr(&self.class_name),
                    make_bare_name_expr(&self.self_name),
                ]);
            }
        }
    }
}

/// First parameter name of a function (positional-only or regular). Returns
/// `None` when the function has no positional parameters at all.
fn first_parameter_name(params: &Parameters) -> Option<String> {
    if let Some(p) = params.posonlyargs.first() {
        return Some(p.parameter.name.as_str().to_owned());
    }
    if let Some(p) = params.args.first() {
        return Some(p.parameter.name.as_str().to_owned());
    }
    None
}

/// Merge Typhon `impl` pseudo-classes into their target classes.
///
/// The preprocessor rewrites `impl ClassName:` to `class __typhon_impl_ClassName(object):`.
/// This function:
/// 1. Finds all such pseudo-classes in `body`.
/// 2. Injects `self` as the first parameter of every method defined inside them.
/// 3. Appends those methods to the corresponding target class body.
/// 4. Removes the pseudo-classes from the statement list.
///
/// Returns the modified statement list and `true` when at least one impl block
/// was found.  Missing target classes are silently skipped (the type checker
/// surfaces a better diagnostic for that case).
fn merge_impl_blocks(body: Vec<Stmt>) -> (Vec<Stmt>, bool) {
    // R2-3: collect sealed-union aliases first so an `impl Union:` block
    // (where `Union` is `type Union = A | B | …`) distributes its methods
    // across every variant class instead of failing to find a single
    // target class.
    let union_aliases = collect_sealed_union_aliases(&body);

    // Phase 1: identify impl pseudo-class indices and their target names.
    let impl_indices: Vec<(usize, &str)> = body
        .iter()
        .enumerate()
        .filter_map(|(i, stmt)| {
            if let Stmt::ClassDef(c) = stmt {
                if let Some(target) = c.name.as_str().strip_prefix(IMPL_PREFIX) {
                    return Some((i, target));
                }
            }
            None
        })
        .collect();

    if impl_indices.is_empty() {
        return (body, false);
    }

    // Names of classes actually declared in this module. An `impl` /
    // `extend` whose target is neither a local class nor a local sealed-
    // union alias refers to an *imported* class (cross-module `extend
    // Record:`), which can't be merged into a class body we don't own —
    // those blocks lower to module-level attribute patches instead
    // (`Record.label = __typhon_extend_Record__label`). Previously they
    // were silently dropped, so the method vanished from the build output.
    let local_classes: HashSet<&str> = body
        .iter()
        .filter_map(|stmt| {
            if let Stmt::ClassDef(c) = stmt {
                let n = c.name.as_str();
                if !n.starts_with(IMPL_PREFIX) {
                    return Some(n);
                }
            }
            None
        })
        .collect();

    // Phase 2: collect methods (with `self` injected) into a map keyed by
    // target class name.  Multiple impl blocks for the same class accumulate.
    //
    // R2-3: if the impl target is a sealed-union alias, replicate the
    // methods under each variant's class name. Each variant ends up with
    // an identical copy of the method body; `match self:` patterns
    // inside the body still dispatch correctly because the runtime
    // class on `self` only matches its own arm. The duplication is a
    // small constant factor in emitted code size for the convenience
    // of writing "the method-on-the-union" once.
    let impl_index_set: HashSet<usize> = impl_indices.iter().map(|(i, _)| *i).collect();
    let mut impl_methods_map: HashMap<String, Vec<Stmt>> = HashMap::new();
    // Pseudo-class indices whose target is foreign: lowered in place to
    // `def __typhon_extend_T__m(self, ...)` + `T.m = __typhon_extend_T__m`.
    let mut foreign_patches: HashMap<usize, Vec<Stmt>> = HashMap::new();
    for (impl_idx, target_name) in &impl_indices {
        if let Stmt::ClassDef(c) = &body[*impl_idx] {
            // PERF OPTIMIZATION: Zero-allocation union aliases
            // By utilizing `&str` references directly from the AST during phase 1, we avoid redundant string cloning during class desugaring.
            let methods: Vec<Stmt> = c.body.iter().map(insert_self_param).collect();
            // Determine the actual target class(es). A union alias expands
            // to every variant; a concrete class is its own target.
            let targets: Vec<&str> = match union_aliases.get(target_name) {
                Some(variants) => variants.clone(),
                None => vec![*target_name],
            };
            let all_local = targets
                .iter()
                .all(|&t| local_classes.contains(t) || union_aliases.contains_key(t));
            if all_local {
                for target in targets {
                    impl_methods_map
                        .entry(target.to_owned())
                        .or_default()
                        .extend(methods.iter().cloned());
                }
            } else {
                foreign_patches.insert(*impl_idx, make_extend_patch_stmts(target_name, &methods));
            }
        }
    }

    // Phase 3: rebuild the body, merging methods into target classes,
    // replacing foreign-target pseudo-classes with their attribute patches
    // (in place, so they run after the import that binds the class), and
    // dropping the local-target pseudo-classes.
    let mut new_body: Vec<Stmt> = Vec::with_capacity(body.len());
    for (i, stmt) in body.into_iter().enumerate() {
        if let Some(patches) = foreign_patches.remove(&i) {
            new_body.extend(patches);
            continue;
        }
        if impl_index_set.contains(&i) {
            continue;
        }
        if let Stmt::ClassDef(mut c) = stmt {
            let name = c.name.as_str().to_owned();
            if let Some(methods) = impl_methods_map.remove(&name) {
                c.body.extend(methods);
            }
            new_body.push(Stmt::ClassDef(c));
        } else {
            new_body.push(stmt);
        }
    }

    (new_body, true)
}

/// Collect, for every top-level class in the module, the map of field name
/// → bare-`Name` annotation text. Only fields whose annotation is a plain
/// identifier are recorded with their target name (that's all the nested
/// dict-literal rewrite needs to recurse); other fields are recorded with
/// an empty string so presence checks still work. Impl pseudo-classes are
/// skipped.
fn collect_local_class_field_annotations(
    body: &[Stmt],
) -> HashMap<String, HashMap<String, String>> {
    let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
    for stmt in body {
        let Stmt::ClassDef(c) = stmt else { continue };
        if c.name.as_str().starts_with(IMPL_PREFIX) {
            continue;
        }
        let mut fields: HashMap<String, String> = HashMap::new();
        for member in &c.body {
            if let Stmt::AnnAssign(a) = member {
                if let Expr::Name(n) = a.target.as_ref() {
                    let ann = match a.annotation.as_ref() {
                        Expr::Name(t) => t.id.as_str().to_owned(),
                        _ => String::new(),
                    };
                    fields.insert(n.id.as_str().to_owned(), ann);
                }
            }
        }
        out.insert(c.name.as_str().to_owned(), fields);
    }
    out
}

/// `true` when `s` is usable as a Python keyword argument name.
fn is_py_identifier(s: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
        "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global",
        "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return",
        "try", "while", "with", "yield",
    ];
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    if !chars.all(|c| c.is_alphanumeric() || c == '_') {
        return false;
    }
    !KEYWORDS.contains(&s)
}

/// Rewrite `{"k": v, ...}` into `Cls(k=v, ...)` when every key is a
/// string-literal identifier naming a field of `Cls`. Recurses into a
/// value that is itself a dict literal when the matching field's
/// annotation names another local class (nested model initialisation).
/// Returns `None` (leave the dict untouched) on any shape we can't
/// prove — the type checker has already validated the match, this pass
/// only performs the syntactic lowering.
fn dict_literal_to_ctor_call(
    d: &ruff_python_ast::ExprDict,
    class_name: &str,
    classes: &HashMap<String, HashMap<String, String>>,
) -> Option<Expr> {
    let fields = classes.get(class_name)?;
    let mut keywords: Vec<ruff_python_ast::Keyword> = Vec::with_capacity(d.items.len());
    for item in &d.items {
        let key_expr = item.key.as_ref()?; // `**spread` — leave untouched.
        let key = match key_expr {
            Expr::StringLiteral(s) => s.value.to_str().to_owned(),
            _ => return None,
        };
        if !is_py_identifier(&key) {
            return None;
        }
        let field_ann = fields.get(&key)?;
        let mut value = item.value.clone();
        if let Expr::Dict(nested) = &value {
            if classes.contains_key(field_ann.as_str()) {
                if let Some(ctor) = dict_literal_to_ctor_call(nested, field_ann, classes) {
                    value = ctor;
                }
            }
        }
        keywords.push(ruff_python_ast::Keyword {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            arg: Some(make_identifier(&key)),
            value,
        });
    }
    Some(Expr::Call(ruff_python_ast::ExprCall {
        range: d.range,
        node_index: AtomicNodeIndex::NONE,
        func: Box::new(make_bare_name_expr(class_name)),
        arguments: ruff_python_ast::Arguments {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            args: Box::from([]),
            keywords: keywords.into_boxed_slice(),
        },
    }))
}

/// Walk every statement (recursing through function bodies and control-flow
/// blocks) rewriting `x: Cls = {dict literal}` value positions via
/// [`dict_literal_to_ctor_call`]. Class bodies are skipped — a dict literal
/// as a *field default* stays a dict.
fn rewrite_typed_dict_literals_in_stmts(
    body: &mut [Stmt],
    classes: &HashMap<String, HashMap<String, String>>,
) {
    for stmt in body.iter_mut() {
        match stmt {
            Stmt::AnnAssign(a) => {
                if let (Expr::Name(ann), Some(value)) = (a.annotation.as_ref(), a.value.as_mut()) {
                    if let Expr::Dict(d) = value.as_ref() {
                        let class_name = ann.id.as_str();
                        if classes.contains_key(class_name) {
                            if let Some(ctor) = dict_literal_to_ctor_call(d, class_name, classes) {
                                **value = ctor;
                            }
                        }
                    }
                }
            }
            Stmt::FunctionDef(f) => rewrite_typed_dict_literals_in_stmts(&mut f.body, classes),
            Stmt::If(s) => {
                rewrite_typed_dict_literals_in_stmts(&mut s.body, classes);
                for clause in s.elif_else_clauses.iter_mut() {
                    rewrite_typed_dict_literals_in_stmts(&mut clause.body, classes);
                }
            }
            Stmt::While(s) => {
                rewrite_typed_dict_literals_in_stmts(&mut s.body, classes);
                rewrite_typed_dict_literals_in_stmts(&mut s.orelse, classes);
            }
            Stmt::For(s) => {
                rewrite_typed_dict_literals_in_stmts(&mut s.body, classes);
                rewrite_typed_dict_literals_in_stmts(&mut s.orelse, classes);
            }
            Stmt::With(s) => rewrite_typed_dict_literals_in_stmts(&mut s.body, classes),
            Stmt::Try(s) => {
                rewrite_typed_dict_literals_in_stmts(&mut s.body, classes);
                for h in s.handlers.iter_mut() {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    rewrite_typed_dict_literals_in_stmts(&mut h.body, classes);
                }
                rewrite_typed_dict_literals_in_stmts(&mut s.orelse, classes);
                rewrite_typed_dict_literals_in_stmts(&mut s.finalbody, classes);
            }
            Stmt::Match(s) => {
                for case in s.cases.iter_mut() {
                    rewrite_typed_dict_literals_in_stmts(&mut case.body, classes);
                }
            }
            _ => {}
        }
    }
}

/// Lower one cross-module `extend Target:` block into module-level patch
/// statements: each method becomes a uniquely-named module function (its
/// decorators preserved, so `@property` and friends still apply) followed
/// by a class-attribute assignment binding it under the method's own name.
/// Class-level attribute assignment is legal on `slots=True` and frozen
/// dataclasses alike — only *instance* attributes are restricted.
fn make_extend_patch_stmts(target: &str, methods: &[Stmt]) -> Vec<Stmt> {
    let mut out: Vec<Stmt> = Vec::new();
    for m in methods {
        let Stmt::FunctionDef(f) = m else {
            // Docstrings / `pass` placeholders inside the block carry no
            // behaviour — drop them.
            continue;
        };
        let method_name = f.name.as_str().to_owned();
        let module_fn_name = format!("__typhon_extend_{target}__{method_name}");
        let mut renamed = f.clone();
        renamed.name = make_identifier(&module_fn_name);
        out.push(Stmt::FunctionDef(renamed));
        // `Target.method = __typhon_extend_Target__method`
        out.push(Stmt::Assign(StmtAssign {
            node_index: AtomicNodeIndex::NONE,
            range: TextRange::default(),
            targets: vec![Expr::Attribute(ExprAttribute {
                node_index: AtomicNodeIndex::NONE,
                range: TextRange::default(),
                value: Box::new(make_bare_name_expr(target)),
                attr: make_identifier(&method_name),
                ctx: ExprContext::Store,
            })],
            value: Box::new(make_bare_name_expr(&module_fn_name)),
            mutability: None,
        }));
    }
    out
}

/// FINDINGS #22: walk every class in `body`, build a `name → field-annotation`
/// map, and prepend the parent class's field annotations to every subclass
/// whose direct base is present in the map. Fields are inserted ahead of
/// the existing class body so they precede any locally-declared field
/// (parent fields first matches CPython's `@dataclass` MRO walk).
///
/// Only `AnnAssign` statements that look like \"field declarations\"
/// (target is a bare `Name`, annotation is present) are propagated. Class
/// methods, docstrings, and class-level assignments are left on the parent
/// only — the VM's `find_method` already walks `class.bases`, so methods
/// are inherited at lookup time without duplication.
///
/// A field declared on both parent and child is kept from the child (the
/// child's annotation wins; the parent's copy is dropped from the
/// prepended block). This matches `@dataclass` field-override semantics.
///
/// Recurses into nested function/class bodies so a class defined inside a
/// function still benefits from this transformation.
fn inherit_parent_fields(body: Vec<Stmt>) -> Vec<Stmt> {
    // First pass: collect each class's own field annotations, indexed by name,
    // plus its direct bases so the MRO can be reconstructed.
    let mut field_map: HashMap<String, Vec<Stmt>> = HashMap::new();
    let mut parents: HashMap<String, Vec<String>> = HashMap::new();
    fn collect_fields(
        stmts: &[Stmt],
        field_map: &mut HashMap<String, Vec<Stmt>>,
        parents: &mut HashMap<String, Vec<String>>,
    ) {
        for stmt in stmts {
            if let Stmt::ClassDef(c) = stmt {
                let mut fields: Vec<Stmt> = Vec::new();
                for member in &c.body {
                    if let Stmt::AnnAssign(a) = member {
                        if matches!(a.target.as_ref(), Expr::Name(_)) {
                            fields.push(member.clone());
                        }
                    }
                }
                field_map.insert(c.name.as_str().to_owned(), fields);
                parents.insert(
                    c.name.as_str().to_owned(),
                    c.bases()
                        .iter()
                        .filter_map(|b| match b {
                            Expr::Name(n) => Some(n.id.as_str().to_owned()),
                            _ => None,
                        })
                        .collect(),
                );
                collect_fields(&c.body, field_map, parents);
            } else if let Stmt::FunctionDef(f) = stmt {
                collect_fields(&f.body, field_map, parents);
            }
        }
    }
    collect_fields(&body, &mut field_map, &mut parents);

    fn rewrite(
        stmts: Vec<Stmt>,
        field_map: &HashMap<String, Vec<Stmt>>,
        parents: &HashMap<String, Vec<String>>,
    ) -> Vec<Stmt> {
        stmts
            .into_iter()
            .map(|stmt| match stmt {
                Stmt::ClassDef(mut c) => {
                    // Recurse so a class defined inside another class
                    // body is also rewritten.
                    let inner = std::mem::take(&mut c.body);
                    c.body = rewrite(inner, field_map, parents);

                    // The class's own field annotations, keyed by name, so a
                    // re-declared parent field can be re-sited (see below).
                    let mut own_fields: HashMap<String, Stmt> = HashMap::new();
                    for s in &c.body {
                        if let Stmt::AnnAssign(a) = s {
                            if let Expr::Name(n) = a.target.as_ref() {
                                own_fields.insert(n.id.as_str().to_owned(), s.clone());
                            }
                        }
                    }

                    // Inherited field order must be **reverse MRO**, because
                    // that is how `@dataclass` builds `__dataclass_fields__`:
                    // it walks `cls.__mro__[-1:0:-1]` updating a dict, then
                    // adds the class's own annotations.
                    //
                    // Walking direct bases left-to-right instead — which is
                    // what this did — silently scrambles the positional
                    // constructor for multiple inheritance. For
                    // `class C(A, B)` with `A(a1: int, a2: str)` and
                    // `B(b1: float)`, the emitted body read `a1, a2, b1, c1`
                    // while `dataclasses.fields()` reported `b1, a1, a2, c1`,
                    // so `C(1, "x", 2.0, True)` wrote the int into `b1`, the
                    // str into `a1` and the float into `a2` — wrong-field
                    // writes, or a TypeError at import, from a green build.
                    //
                    // A re-declared parent field keeps the *parent's*
                    // position (a dict `update` on an existing key does not
                    // move it) while taking the *child's* annotation, so it is
                    // emitted here and removed from the child's own block.
                    let ancestors =
                        tyc_syntax::mro::field_collection_order(c.name.as_str(), parents);

                    let mut inherited: Vec<Stmt> = Vec::new();
                    let mut placed: std::collections::HashSet<String> =
                        std::collections::HashSet::new();
                    for ancestor in &ancestors {
                        let Some(parent_fields) = field_map.get(ancestor) else {
                            continue;
                        };
                        for pf in parent_fields {
                            let Stmt::AnnAssign(a) = pf else { continue };
                            let Expr::Name(nf) = a.target.as_ref() else {
                                continue;
                            };
                            let fname = nf.id.as_str();
                            if !placed.insert(fname.to_owned()) {
                                continue;
                            }
                            // A child re-declaration overrides the type but
                            // not the position.
                            inherited
                                .push(own_fields.get(fname).cloned().unwrap_or_else(|| pf.clone()));
                        }
                    }
                    // Drop the child's own copies of re-declared fields; they
                    // have been re-sited into `inherited` at the parent's
                    // position.
                    if !inherited.is_empty() {
                        c.body.retain(|s| match s {
                            Stmt::AnnAssign(a) => match a.target.as_ref() {
                                Expr::Name(n) => !placed.contains(n.id.as_str()),
                                _ => true,
                            },
                            _ => true,
                        });
                    }

                    if !inherited.is_empty() {
                        // FINDINGS #22: a `class!` subclass gets a synthesised
                        // `__init__` during `desugar_stmts`, *before* these
                        // inherited fields are injected — so that constructor
                        // only accepts the child's own fields and calls a
                        // no-arg `super().__init__()`. For an in-module base
                        // (the only kind in `field_map`) that no-arg super
                        // call hits the base's field-requiring dataclass
                        // `__init__` and raises `TypeError` at runtime, and the
                        // inherited fields can't be passed at all. Rewrite the
                        // synthesised constructor here so it accepts the
                        // inherited fields and assigns them directly, dropping
                        // the now-broken no-arg super call. Framework bases are
                        // never in `field_map`, so their synthesised inits
                        // (which legitimately need `super().__init__()`) are
                        // left untouched.
                        patch_synthesised_init_with_inherited_fields(&mut c.body, &inherited);
                        // Insert inherited fields after any leading
                        // docstring so `__doc__` is preserved.
                        let insert_at = if let Some(Stmt::Expr(e)) = c.body.first() {
                            if matches!(&*e.value, Expr::StringLiteral(_)) {
                                1
                            } else {
                                0
                            }
                        } else {
                            0
                        };
                        for (i, field) in inherited.into_iter().enumerate() {
                            c.body.insert(insert_at + i, field);
                        }
                    }
                    Stmt::ClassDef(c)
                }
                Stmt::FunctionDef(mut f) => {
                    let inner = std::mem::take(&mut f.body);
                    f.body = rewrite(inner, field_map, parents);
                    Stmt::FunctionDef(f)
                }
                other => other,
            })
            .collect()
    }
    rewrite(body, &field_map, &parents)
}

/// Rewrite a `class!`'s synthesised `__init__` so it also accepts and
/// assigns fields inherited from an in-module base.
///
/// `inherited` is the list of parent-field `AnnAssign` statements about to
/// be prepended to the class body. The synthesised constructor was built
/// during `desugar_stmts` *before* those fields were visible, so it only
/// covers the child's own fields and opens with a no-arg
/// `super().__init__()` (rewritten to the two-arg `super(C, self).__init__()`
/// form by the earlier `rewrite_bare_super` pass). That no-arg call hits the
/// base's field-requiring dataclass constructor and raises `TypeError`, so we
/// drop it and assign every field directly instead.
///
/// Only an `__init__` whose first statement is exactly that argument-less
/// `super(...).__init__()` call is touched — that uniquely identifies the
/// synthesised field-assigning constructor and leaves user-written and
/// `*args/**kwargs` passthrough constructors untouched.
fn patch_synthesised_init_with_inherited_fields(body: &mut [Stmt], inherited: &[Stmt]) {
    // Pull (name, annotation, default) out of each inherited field.
    let inherited_fields: Vec<(Name, Expr, Option<Expr>)> = inherited
        .iter()
        .filter_map(|s| {
            if let Stmt::AnnAssign(a) = s {
                if let Expr::Name(n) = a.target.as_ref() {
                    return Some((
                        n.id.clone(),
                        (*a.annotation).clone(),
                        a.value.as_deref().cloned(),
                    ));
                }
            }
            None
        })
        .collect();
    if inherited_fields.is_empty() {
        return;
    }

    for stmt in body.iter_mut() {
        let Stmt::FunctionDef(f) = stmt else { continue };
        if f.name.as_str() != "__init__" {
            continue;
        }
        // Match only the synthesised field-assigning constructor: an argless
        // `super(...).__init__()` followed solely by `self.x = x` assignments.
        // A hand-written `__init__` with any other logic is left untouched.
        if !is_synthesised_field_init(&f.body) {
            continue;
        }

        // Build inherited parameters (these carry their own defaults).
        let inherited_params: Vec<ParameterWithDefault> = inherited_fields
            .iter()
            .map(|(name, annotation, default)| ParameterWithDefault {
                range: TextRange::default(),
                node_index: AtomicNodeIndex::NONE,
                parameter: Parameter {
                    range: TextRange::default(),
                    node_index: AtomicNodeIndex::NONE,
                    name: make_identifier(name.as_str()),
                    annotation: Some(Box::new(annotation.clone())),
                },
                default: default.as_ref().map(|d| Box::new(d.clone())),
            })
            .collect();

        // Splice inherited params after `self`, then stable-partition the
        // whole list so non-defaulted params precede defaulted ones (Python
        // forbids a required parameter after a defaulted one).
        let existing: Vec<ParameterWithDefault> = std::mem::take(&mut f.parameters.args);
        let mut self_param: Vec<ParameterWithDefault> = Vec::new();
        let mut rest: Vec<ParameterWithDefault> = Vec::new();
        for (i, p) in existing.into_iter().enumerate() {
            if i == 0 && p.parameter.name.as_str() == "self" {
                self_param.push(p);
            } else {
                rest.push(p);
            }
        }
        let mut combined: Vec<ParameterWithDefault> = inherited_params;
        combined.extend(rest);
        let (no_default, with_default): (Vec<_>, Vec<_>) =
            combined.into_iter().partition(|p| p.default.is_none());
        let mut new_args = self_param;
        new_args.extend(no_default);
        new_args.extend(with_default);
        f.parameters.args = new_args;

        // Drop the leading no-arg `super(...).__init__()` and prepend
        // `self.<field> = <field>` for each inherited field (parent order).
        if !f.body.is_empty() {
            f.body.remove(0);
        }
        for (i, (name, _, _)) in inherited_fields.iter().enumerate() {
            f.body.insert(i, make_self_field_assign_stmt(name.as_str()));
        }
        // Only one `__init__` per class body.
        break;
    }
}

/// `true` when the first statement of a function body is an argument-less
/// `super(...).__init__()` expression statement — the marker of a
/// synthesised field-assigning `class!` constructor.
fn first_stmt_is_argless_super_init(body: &[Stmt]) -> bool {
    let Some(Stmt::Expr(e)) = body.first() else {
        return false;
    };
    let Expr::Call(call) = e.value.as_ref() else {
        return false;
    };
    // No arguments to `__init__(...)`.
    if !call.arguments.args.is_empty() || !call.arguments.keywords.is_empty() {
        return false;
    }
    // `<super-call>.__init__`
    let Expr::Attribute(attr) = call.func.as_ref() else {
        return false;
    };
    if attr.attr.as_str() != "__init__" {
        return false;
    }
    // The receiver is a call to `super` (bare `super()` or `super(C, self)`).
    let Expr::Call(super_call) = attr.value.as_ref() else {
        return false;
    };
    matches!(super_call.func.as_ref(), Expr::Name(n) if n.id.as_str() == "super")
}

/// True only for the EXACT shape the desugarer synthesises for a `class!` /
/// exception-with-fields constructor: an argless `super().__init__()` followed
/// by zero or more plain `self.NAME = NAME` field assignments and nothing
/// else. A hand-written `__init__` (which `plain class` / `class!` permit) that
/// carries any other logic — a literal/computed assignment, a method call, a
/// conditional — does NOT match, so the inherited-field rewrite never alters or
/// drops user code or skips arbitrary base initialisation.
fn is_synthesised_field_init(body: &[Stmt]) -> bool {
    if !first_stmt_is_argless_super_init(body) {
        return false;
    }
    body[1..].iter().all(|s| {
        let Stmt::Assign(a) = s else {
            return false;
        };
        if a.targets.len() != 1 {
            return false;
        }
        let Expr::Attribute(attr) = &a.targets[0] else {
            return false;
        };
        let Expr::Name(recv) = attr.value.as_ref() else {
            return false;
        };
        if recv.id.as_str() != "self" {
            return false;
        }
        // RHS must be the bare parameter of the same name (`self.x = x`).
        matches!(a.value.as_ref(), Expr::Name(val) if val.id.as_str() == attr.attr.as_str())
    })
}

/// Inject an implicit receiver parameter into a method from an `impl` block.
///
/// Handles `Stmt::FunctionDef` (which now subsumes async functions via the
/// `is_async` flag); all other statement kinds pass through unchanged.
///
/// Rules:
/// - `@staticmethod`: no parameter is injected (static methods have no receiver).
/// - `@classmethod`: `cls` is injected as the first parameter.
/// - All other methods: `self` is injected as the first parameter.
///
/// The parameter is inserted into `posonlyargs` when the method already has
/// positional-only arguments (preserving correct parameter order around `/`),
/// or into `args` otherwise.  If the receiver name is already present as the
/// first parameter, it is not duplicated.
fn insert_self_param(stmt: &Stmt) -> Stmt {
    fn make_receiver_param(name: &'static str) -> ParameterWithDefault {
        ParameterWithDefault {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            parameter: Parameter {
                range: TextRange::default(),
                node_index: AtomicNodeIndex::NONE,
                name: make_identifier(name),
                annotation: None,
            },
            default: None,
        }
    }

    fn first_param_name(params: &Parameters) -> Option<&str> {
        params
            .posonlyargs
            .first()
            .or_else(|| params.args.first())
            .map(|p| p.parameter.name.as_str())
    }

    fn decorator_name(d: &Expr) -> Option<&str> {
        match d {
            Expr::Name(n) => Some(n.id.as_str()),
            Expr::Call(c) => decorator_name(&c.func),
            _ => None,
        }
    }

    fn receiver_for(decorators: &[Decorator]) -> Option<&'static str> {
        for d in decorators {
            match decorator_name(&d.expression) {
                Some("staticmethod") => return None, // no receiver
                Some("classmethod") => return Some("cls"),
                _ => {}
            }
        }
        Some("self")
    }

    fn inject(params: &mut Parameters, receiver: &'static str) {
        if first_param_name(params) == Some(receiver) {
            return; // already present — do not duplicate
        }
        let param = make_receiver_param(receiver);
        if params.posonlyargs.is_empty() {
            params.args.insert(0, param);
        } else {
            params.posonlyargs.insert(0, param);
        }
    }

    match stmt {
        Stmt::FunctionDef(f) => {
            let mut new_f = f.clone();
            if let Some(receiver) = receiver_for(&new_f.decorator_list) {
                inject(&mut new_f.parameters, receiver);
            }
            Stmt::FunctionDef(new_f)
        }
        other => other.clone(),
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tyc_emit::emit;

    fn parse_and_desugar(src: &str) -> String {
        let parsed = tyc_syntax::parse_module(src).expect("parse failed");
        let module = parsed.into_syntax();
        let output = desugar_module(&module);
        emit(&output.module)
    }

    #[test]
    fn plain_class_gets_dataclass_decorator() {
        let src = "class Point:\n    x: int\n    y: int\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("@dataclasses.dataclass(slots=True)"),
            "output:\n{out}"
        );
        assert!(out.contains("import dataclasses"), "output:\n{out}");
    }

    fn raw_class_starts_for(src: &str) -> (ModModule, Vec<u32>) {
        let parsed = tyc_syntax::parse_module(src).expect("parse failed");
        let module = parsed.into_syntax();
        let starts: Vec<u32> = module
            .body
            .iter()
            .filter_map(|s| match s {
                Stmt::ClassDef(c) => Some(u32::from(c.range.start())),
                _ => None,
            })
            .collect();
        (module, starts)
    }

    #[test]
    fn raw_class_with_fields_synthesises_init_with_super() {
        let src = "class Net(Module):\n    layer: int\n    dropout: float\n";
        let (module, starts) = raw_class_starts_for(src);
        let out = desugar_module_with(
            &module,
            DesugarOptions {
                memoise_functions: Vec::new(),
                raw_class_line_starts: starts,
                frozen_class_line_starts: Vec::new(),
                ..Default::default()
            },
        );
        let emitted = emit(&out.module);
        assert!(
            emitted.contains("def __init__(self, layer: int, dropout: float) -> None:"),
            "expected synthesised __init__ signature:\n{emitted}"
        );
        // The synthesised bare `super().__init__()` is rewritten to the
        // explicit two-argument form by the H0 `super()` pass (correct and
        // equivalent; works whether or not the class is `@dataclass`).
        assert!(
            emitted.contains("super(Net, self).__init__()"),
            "expected two-arg super() call:\n{emitted}"
        );
        assert!(
            emitted.contains("self.layer = layer"),
            "expected self.layer assignment:\n{emitted}"
        );
        assert!(
            emitted.contains("self.dropout = dropout"),
            "expected self.dropout assignment:\n{emitted}"
        );
        // Annotations stay above the synthesised init (canonical layout).
        let init_pos = emitted.find("def __init__").unwrap();
        let layer_pos = emitted.find("layer: int").unwrap();
        assert!(
            layer_pos < init_pos,
            "fields should precede __init__:\n{emitted}"
        );
    }

    #[test]
    fn raw_class_with_decorator_still_recognised() {
        // Ruff sets `StmtClassDef.range.start()` to the `@` token when
        // decorators are present, but the preprocessor records the
        // `class` keyword line. The desugar pass must look at the
        // header span (start..name) rather than the node start, or this
        // raw class would silently receive `@dataclass`. The source
        // here is the *post-preprocess* form (the `!` is already
        // stripped), matching what `tyc_syntax::parse_module` sees.
        let src = "@deco\nclass Net(Module):\n    rank: int\n";
        let parsed = tyc_syntax::parse_module(src).expect("parse failed");
        let module = parsed.into_syntax();
        // Match what `tyc_syntax::preprocess::line_byte_starts` would
        // emit for the `class!` line: the byte offset of the `class`
        // keyword (after leading whitespace).
        let class_kw = src.find("class").unwrap() as u32;
        let out = desugar_module_with(
            &module,
            DesugarOptions {
                memoise_functions: Vec::new(),
                raw_class_line_starts: vec![class_kw],
                frozen_class_line_starts: Vec::new(),
                ..Default::default()
            },
        );
        let emitted = emit(&out.module);
        assert!(
            !emitted.contains("@dataclasses.dataclass"),
            "decorated raw class must skip @dataclass:\n{emitted}"
        );
        assert!(
            emitted.contains("def __init__(self, rank: int) -> None:"),
            "decorated raw class must still get the synthesised __init__:\n{emitted}"
        );
        assert!(
            emitted.contains("@deco"),
            "the author's decorator must survive:\n{emitted}"
        );
    }

    #[test]
    fn raw_class_defaults_partition_to_keep_signature_valid() {
        // Source order is x (default) → y (no default) → z (no default).
        // Naive emission would produce `def __init__(self, x=1, y, z)`,
        // which is a SyntaxError. The synthesis must stable-partition so
        // non-defaulted params come first.
        let src = "class Net(Module):\n    x: int = 1\n    y: int\n    z: str\n";
        let (module, starts) = raw_class_starts_for(src);
        let out = desugar_module_with(
            &module,
            DesugarOptions {
                memoise_functions: Vec::new(),
                raw_class_line_starts: starts,
                frozen_class_line_starts: Vec::new(),
                ..Default::default()
            },
        );
        let emitted = emit(&out.module);
        assert!(
            emitted.contains("def __init__(self, y: int, z: str, x: int = 1) -> None:"),
            "synthesised __init__ must reorder defaults to the tail:\n{emitted}"
        );
        // Assignment block stays in source order (reads top-to-bottom).
        let body_order = emitted
            .lines()
            .filter_map(|l| {
                let t = l.trim();
                if let Some(rest) = t.strip_prefix("self.") {
                    rest.split(' ').next()
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(
            body_order,
            vec!["x", "y", "z"],
            "self.<field> assignments must follow source order:\n{emitted}"
        );
    }

    #[test]
    fn raw_class_default_value_flows_into_init_signature() {
        let src = "class Net(Module):\n    width: int\n    height: int = 100\n";
        let (module, starts) = raw_class_starts_for(src);
        let out = desugar_module_with(
            &module,
            DesugarOptions {
                memoise_functions: Vec::new(),
                raw_class_line_starts: starts,
                frozen_class_line_starts: Vec::new(),
                ..Default::default()
            },
        );
        let emitted = emit(&out.module);
        assert!(
            emitted.contains("def __init__(self, width: int, height: int = 100) -> None:"),
            "expected default value in synthesised __init__:\n{emitted}"
        );
    }

    #[test]
    fn raw_class_with_explicit_init_is_left_alone() {
        let src =
            "class Net(Module):\n    width: int\n\n    def __init__(self, width: int) -> None:\n        self.width = width * 2\n";
        let (module, starts) = raw_class_starts_for(src);
        let out = desugar_module_with(
            &module,
            DesugarOptions {
                memoise_functions: Vec::new(),
                raw_class_line_starts: starts,
                frozen_class_line_starts: Vec::new(),
                ..Default::default()
            },
        );
        let emitted = emit(&out.module);
        // Only ONE __init__: the user's.
        assert_eq!(
            emitted.matches("def __init__").count(),
            1,
            "explicit __init__ must not be duplicated:\n{emitted}"
        );
        // The user's body is preserved.
        assert!(
            emitted.contains("self.width = width * 2"),
            "user __init__ body must be preserved:\n{emitted}"
        );
    }

    #[test]
    fn raw_class_without_bases_skips_synthesis() {
        // No bases → no super() chain to invoke; we leave the class alone.
        let src = "class Standalone:\n    x: int\n";
        let (module, starts) = raw_class_starts_for(src);
        let out = desugar_module_with(
            &module,
            DesugarOptions {
                memoise_functions: Vec::new(),
                raw_class_line_starts: starts,
                frozen_class_line_starts: Vec::new(),
                ..Default::default()
            },
        );
        let emitted = emit(&out.module);
        assert!(
            !emitted.contains("def __init__"),
            "no __init__ should be synthesised for a raw class with no bases:\n{emitted}"
        );
        // Also: no dataclass decorator (since `class!` was set).
        assert!(
            !emitted.contains("@dataclasses.dataclass"),
            "raw class must still skip @dataclass:\n{emitted}"
        );
    }

    #[test]
    fn bare_super_rewritten_to_two_arg_form() {
        // A user-written bare `super()` inside a method becomes
        // `super(ClassName, self)` so it survives `@dataclass(slots=True)`.
        let src = "class B(A):\n    def m(self) -> str:\n        return super().m()\n";
        let parsed = tyc_syntax::parse_module(src).expect("parse failed");
        let module = parsed.into_syntax();
        let out = desugar_module_with(&module, DesugarOptions::default());
        let emitted = emit(&out.module);
        assert!(
            emitted.contains("super(B, self).m()"),
            "bare super() should become super(B, self):\n{emitted}"
        );
        assert!(
            !emitted.contains("super().m()"),
            "no bare super().m() should remain:\n{emitted}"
        );
    }

    #[test]
    fn explicit_two_arg_super_left_untouched() {
        let src = "class B(A):\n    def m(self) -> str:\n        return super(B, self).m()\n";
        let parsed = tyc_syntax::parse_module(src).expect("parse failed");
        let module = parsed.into_syntax();
        let out = desugar_module_with(&module, DesugarOptions::default());
        let emitted = emit(&out.module);
        // Still exactly one super(B, self) — not double-rewritten.
        assert_eq!(
            emitted.matches("super(B, self)").count(),
            1,
            "explicit super(B, self) must be left as-is:\n{emitted}"
        );
    }

    #[test]
    fn enum_class_gets_enum_import_and_no_dataclass() {
        // Simulate the `enum` preprocessor output.
        let src = "class Shape(enum.Enum):\n    CIRCLE = enum.auto()\n    SQUARE = enum.auto()\n";
        let parsed = tyc_syntax::parse_module(src).expect("parse failed");
        let module = parsed.into_syntax();
        let out = desugar_module_with(&module, DesugarOptions::default());
        let emitted = emit(&out.module);
        assert!(
            emitted.contains("import enum"),
            "enum class must trigger `import enum`:\n{emitted}"
        );
        assert!(
            !emitted.contains("@dataclasses.dataclass"),
            "enum.Enum subclass must skip @dataclass:\n{emitted}"
        );
    }

    #[test]
    fn plain_class_unaffected_by_init_synthesis() {
        // Plain `class` (not `class!`) keeps its dataclass treatment and
        // never gets a hand-rolled __init__ injected.
        let src = "class Plain(Base):\n    x: int\n";
        let parsed = tyc_syntax::parse_module(src).expect("parse failed");
        let module = parsed.into_syntax();
        let out = desugar_module_with(&module, DesugarOptions::default());
        let emitted = emit(&out.module);
        assert!(
            emitted.contains("@dataclasses.dataclass"),
            "plain class should still receive @dataclass:\n{emitted}"
        );
        assert!(
            !emitted.contains("def __init__"),
            "plain class must NOT get a synthesised __init__:\n{emitted}"
        );
    }

    #[test]
    fn raw_class_skips_dataclass_decorator() {
        // Simulate the preprocessor output: `class!` is stripped to `class`
        // and the line index is recorded in `raw_class_line_starts`.  The
        // desugar pass should leave the class alone — no `@dataclass`, no
        // injected `import dataclasses`.
        let src = "class MyModel:\n    name: str\n";
        let parsed = tyc_syntax::parse_module(src).expect("parse failed");
        let module = parsed.into_syntax();
        let class_start = match &module.body[0] {
            Stmt::ClassDef(c) => u32::from(c.range.start()),
            _ => panic!("expected class def"),
        };
        let out = desugar_module_with(
            &module,
            DesugarOptions {
                memoise_functions: Vec::new(),
                raw_class_line_starts: vec![class_start],
                frozen_class_line_starts: Vec::new(),
                ..Default::default()
            },
        );
        let emitted = emit(&out.module);
        assert!(
            !emitted.contains("@dataclasses.dataclass"),
            "raw class must not receive @dataclass:\n{emitted}"
        );
        assert!(
            !emitted.contains("import dataclasses"),
            "raw class must not trigger dataclasses import:\n{emitted}"
        );
        assert!(emitted.contains("class MyModel:"), "output:\n{emitted}");
    }

    // ── plain class keyword ─────────────────────────────────────────────────

    #[test]
    fn plain_class_skips_dataclass_decorator_and_init_synthesis() {
        // The post-preprocess source already has `plain ` stripped — the
        // preprocessor only forwards the recorded line offset.
        let src = "class Net(Module):\n    layer: int\n    dropout: float\n";
        let parsed = tyc_syntax::parse_module(src).expect("parse failed");
        let module = parsed.into_syntax();
        let class_start = match &module.body[0] {
            Stmt::ClassDef(c) => u32::from(c.range.start()),
            _ => panic!("expected class def"),
        };
        let out = desugar_module_with(
            &module,
            DesugarOptions {
                plain_class_line_starts: vec![class_start],
                ..Default::default()
            },
        );
        let emitted = emit(&out.module);
        assert!(
            !emitted.contains("@dataclasses.dataclass"),
            "plain class must skip @dataclass:\n{emitted}"
        );
        // Plain class MUST NOT synthesise an __init__ even though it has
        // a base class — the body is emitted verbatim.
        assert!(
            !emitted.contains("def __init__"),
            "plain class must not synthesise __init__:\n{emitted}"
        );
        // Field annotations survive as-is.
        assert!(
            emitted.contains("layer: int"),
            "plain class fields must be preserved:\n{emitted}"
        );
    }

    #[test]
    fn plain_class_without_bases_emits_nothing_extra() {
        let src = "class App:\n    x: int\n";
        let parsed = tyc_syntax::parse_module(src).expect("parse failed");
        let module = parsed.into_syntax();
        let class_start = match &module.body[0] {
            Stmt::ClassDef(c) => u32::from(c.range.start()),
            _ => panic!("expected class def"),
        };
        let out = desugar_module_with(
            &module,
            DesugarOptions {
                plain_class_line_starts: vec![class_start],
                ..Default::default()
            },
        );
        let emitted = emit(&out.module);
        assert!(
            !emitted.contains("@dataclasses.dataclass"),
            "plain class must skip @dataclass:\n{emitted}"
        );
        assert!(
            !emitted.contains("def __init__"),
            "plain class must not synthesise __init__:\n{emitted}"
        );
        assert!(
            !emitted.contains("import dataclasses"),
            "plain class must not trigger dataclasses import:\n{emitted}"
        );
    }

    // ── auto-skip dataclass for non-dataclass parents ──────────────────────

    #[test]
    fn enum_subclass_skips_dataclass_decorator() {
        let src = "class Color(Enum):\n    RED = 1\n    BLUE = 2\n";
        let out = parse_and_desugar(src);
        assert!(
            !out.contains("@dataclasses.dataclass"),
            "Enum subclass must not get @dataclass:\n{out}"
        );
    }

    #[test]
    fn int_enum_subclass_skips_dataclass_decorator() {
        let src = "class Level(IntEnum):\n    LOW = 0\n    HIGH = 1\n";
        let out = parse_and_desugar(src);
        assert!(
            !out.contains("@dataclasses.dataclass"),
            "IntEnum subclass must not get @dataclass:\n{out}"
        );
    }

    #[test]
    fn qualified_enum_subclass_skips_dataclass_decorator() {
        let src = "class Color(enum.Enum):\n    RED = 1\n    BLUE = 2\n";
        let out = parse_and_desugar(src);
        assert!(
            !out.contains("@dataclasses.dataclass"),
            "enum.Enum subclass must not get @dataclass:\n{out}"
        );
    }

    #[test]
    fn abc_subclass_skips_dataclass_decorator() {
        let src = "class Animal(ABC):\n    name: str\n";
        let out = parse_and_desugar(src);
        assert!(
            !out.contains("@dataclasses.dataclass"),
            "ABC subclass must not get @dataclass:\n{out}"
        );
    }

    #[test]
    fn user_listed_skip_base_suppresses_dataclass_decorator() {
        let src = "class T(App):\n    name: str\n";
        let parsed = tyc_syntax::parse_module(src).expect("parse failed");
        let module = parsed.into_syntax();
        let out = desugar_module_with(
            &module,
            DesugarOptions {
                skip_decoration_bases: vec!["App".into()],
                ..Default::default()
            },
        );
        let emitted = emit(&out.module);
        assert!(
            !emitted.contains("@dataclasses.dataclass"),
            "user-listed base must suppress @dataclass:\n{emitted}"
        );
    }

    #[test]
    fn user_listed_skip_base_matches_last_segment() {
        let src = "class T(textual.App):\n    name: str\n";
        let parsed = tyc_syntax::parse_module(src).expect("parse failed");
        let module = parsed.into_syntax();
        let out = desugar_module_with(
            &module,
            DesugarOptions {
                skip_decoration_bases: vec!["App".into()],
                ..Default::default()
            },
        );
        let emitted = emit(&out.module);
        assert!(
            !emitted.contains("@dataclasses.dataclass"),
            "user-listed base must match by last segment:\n{emitted}"
        );
    }

    #[test]
    fn unrelated_base_still_gets_dataclass_decorator() {
        let src = "class T(SomeRegularBase):\n    name: str\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("@dataclasses.dataclass"),
            "unrelated base must still get @dataclass:\n{out}"
        );
    }

    #[test]
    fn exception_subclass_skips_dataclass_decorator() {
        // `class FooError(Exception): pass` must NOT get `@dataclass` — the
        // synthesised no-arg `__init__` would shadow `BaseException.__init__`
        // and break `raise FooError("msg")`.
        let out = parse_and_desugar("class FooError(Exception):\n    pass\n");
        assert!(
            !out.contains("@dataclasses.dataclass"),
            "exception subclass must not get @dataclass:\n{out}"
        );
        // Field-less exception stays bare (inherits BaseException.__init__):
        // no synthesised `__init__`.
        assert!(
            !out.contains("def __init__"),
            "field-less exception must not synthesise __init__:\n{out}"
        );
    }

    #[test]
    fn exception_subclass_via_user_hierarchy_skips_dataclass() {
        // `AppError(Exception)` then `NotFoundError(AppError)` — both are
        // exceptions by the `*Error` naming convention.
        let src =
            "class AppError(Exception):\n    pass\n\nclass NotFoundError(AppError):\n    pass\n";
        let out = parse_and_desugar(src);
        assert!(
            !out.contains("@dataclasses.dataclass"),
            "exception hierarchy must not get @dataclass:\n{out}"
        );
    }

    #[test]
    fn exception_subclass_via_nonsuffixed_user_base_skips_dataclass() {
        // `Failure(Exception)` is an exception (base ends in `Exception`),
        // but `Timeout(Failure)` has a base that does NOT match the suffix
        // heuristic — the transitive module pass must still recognise it so
        // `Timeout("msg")` inherits `BaseException.__init__`.
        let src = "class Failure(Exception):\n    pass\n\nclass Timeout(Failure):\n    pass\n";
        let out = parse_and_desugar(src);
        assert!(
            !out.contains("@dataclasses.dataclass"),
            "subclass of a non-suffixed exception base must not get @dataclass:\n{out}"
        );
    }

    #[test]
    fn exception_subclass_with_fields_synthesises_init() {
        // An exception that declares fields and no manual __init__ gets a
        // `class!`-style constructor (super().__init__() + field assigns).
        let src = "class HttpError(Exception):\n    code: int\n    detail: str\n";
        let out = parse_and_desugar(src);
        assert!(
            !out.contains("@dataclasses.dataclass"),
            "exception with fields must not get @dataclass:\n{out}"
        );
        assert!(
            out.contains("def __init__(self, code: int, detail: str)"),
            "exception with fields must synthesise a field-assigning __init__:\n{out}"
        );
        assert!(
            out.contains("self.code = code") && out.contains("self.detail = detail"),
            "synthesised __init__ must assign fields:\n{out}"
        );
    }

    #[test]
    fn synthesised_init_subclass_inits_inherited_inmodule_fields() {
        // Regression: a synthesised-`__init__` class (here a child exception
        // whose in-module base also carries fields and a synthesised init)
        // used to get a constructor that only accepted its OWN field and
        // opened with a no-arg `super().__init__()`. That super call hits the
        // base's field-requiring constructor and raises TypeError at runtime —
        // and the inherited field could not be passed at all. The constructor
        // must instead accept the inherited fields and assign them directly,
        // with no super call. (Same code path the `class!` reproducer hits via
        // the raw-class marker, which is not visible at the desugar unit
        // level.)
        let src = "class BaseErr(Exception):\n    code: int\n\nclass ChildErr(BaseErr):\n    detail: str\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("def __init__(self, code: int, detail: str)"),
            "synthesised __init__ must accept inherited fields:\n{out}"
        );
        assert!(
            out.contains("self.code = code") && out.contains("self.detail = detail"),
            "synthesised __init__ must assign every inherited+own field:\n{out}"
        );
    }

    #[test]
    fn hand_written_init_with_custom_logic_is_not_rewritten() {
        // Regression: the inherited-field rewrite must only touch the
        // SYNTHESISED constructor (argless `super().__init__()` + `self.x = x`
        // assignments). A hand-written `__init__` carrying any other logic must
        // be left exactly as written — its signature unchanged, its super call
        // and custom statements preserved.
        let src = "class BaseErr(Exception):\n    code: int\n\nclass ChildErr(BaseErr):\n    detail: str\n    def __init__(self) -> None:\n        super().__init__()\n        self.detail = \"x\"\n        print(\"custom\")\n";
        let out = parse_and_desugar(src);
        // Inspect ChildErr's body specifically — BaseErr legitimately gets a
        // synthesised `__init__(self, code)`.
        let child = out
            .split_once("class ChildErr")
            .map(|(_, rest)| rest)
            .unwrap_or(out.as_str());
        // A rewritten init would read `def __init__(self, code: int, …)`; the
        // hand-written one keeps its exact `(self)` signature, super call, and
        // custom statement.
        assert!(
            child.contains("def __init__(self) -> None:"),
            "hand-written __init__ signature must be unchanged (no spliced fields):\n{out}"
        );
        assert!(
            child.contains("print(\"custom\")") && child.contains("super("),
            "hand-written __init__ custom logic and super call must be preserved:\n{out}"
        );
    }

    #[test]
    fn nested_exception_does_not_taint_toplevel_dataclass() {
        // A function-local `class Failure(Exception):` must NOT mark a
        // top-level `class Failure:` dataclass as an exception (different
        // scope). The top-level one keeps `@dataclass`; the nested one is
        // still detected as an exception via its external base.
        let src = "\
class Failure:
    code: int

def helper() -> None:
    class Failure(Exception):
        pass
";
        let out = parse_and_desugar(src);
        // The top-level dataclass `Failure` keeps its decorator; the nested
        // `Failure(Exception)` does not get one.
        assert!(
            out.contains("@dataclasses.dataclass"),
            "top-level Failure dataclass must keep @dataclass:\n{out}"
        );
        assert_eq!(
            out.matches("@dataclasses.dataclass").count(),
            1,
            "only the top-level dataclass should be decorated:\n{out}"
        );
    }

    #[test]
    fn subclass_of_error_named_dataclass_stays_dataclass() {
        // `LexError` (no base) is a Result error-variant dataclass; a subclass
        // `Detailed(LexError)` must ALSO stay a dataclass (inherit fields),
        // not be mistaken for an exception by the `*Error` base name.
        let src = "class LexError:\n    line: int\n\nclass Detailed(LexError):\n    code: int\n";
        let out = parse_and_desugar(src);
        assert_eq!(
            out.matches("@dataclasses.dataclass").count(),
            2,
            "both the error-named dataclass and its subclass must keep @dataclass:\n{out}"
        );
    }

    #[test]
    fn error_named_class_without_base_still_dataclass() {
        // `class LexError:` with NO base is a Result error *variant*, not an
        // exception — it must keep its `@dataclass` shape.
        let out = parse_and_desugar("class LexError:\n    line: int\n    message: str\n");
        assert!(
            out.contains("@dataclasses.dataclass"),
            "error-named class with no base must stay a dataclass:\n{out}"
        );
    }

    #[test]
    fn class_with_existing_bare_dataclass_not_duplicated() {
        let src = "from dataclasses import dataclass\n\n@dataclass\nclass Point:\n    x: int\n";
        let out = parse_and_desugar(src);
        assert!(!out.contains("slots=True"), "output:\n{out}");
    }

    #[test]
    fn class_with_qualified_dataclass_not_duplicated() {
        let src = "import dataclasses\n\n@dataclasses.dataclass\nclass Point:\n    x: int\n";
        let out = parse_and_desugar(src);
        assert!(!out.contains("slots=True"), "output:\n{out}");
        // No second import injected.
        assert_eq!(
            out.matches("import dataclasses").count(),
            1,
            "output:\n{out}"
        );
    }

    #[test]
    fn class_with_qualified_dataclass_call_not_duplicated() {
        let src =
            "import dataclasses\n\n@dataclasses.dataclass(slots=True)\nclass Point:\n    x: int\n";
        let out = parse_and_desugar(src);
        assert_eq!(
            out.matches("@dataclasses.dataclass").count(),
            1,
            "output:\n{out}"
        );
    }

    #[test]
    fn match_pattern_only_triggers_result_import() {
        // PR #120 follow-up: a module that only ever matches on a Result
        // (never returns / constructs one) used to skip the auto-import,
        // so the emitted Python NameError'd at runtime on `case Ok(...)`.
        // The fix walks each case pattern, not just the case body.
        let src = "def handle() -> None:\n    \
                   value = lookup()\n    \
                   match value:\n        \
                   case Ok(v):\n            \
                   print(v)\n        \
                   case Err(e):\n            \
                   print(e)\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("from typhon_runtime import Ok, Err, Result"),
            "match-only Ok/Err reference must inject the runtime import:\n{out}"
        );
    }

    #[test]
    fn try_result_use_injects_its_runtime_import() {
        // Using `try_result` (with no bare Ok/Err/Result reference) injects
        // `from typhon_runtime import try_result` independently — and does NOT
        // pull in the Ok/Err/Result family import when those aren't used.
        let src = "def f() -> None:\n    try_result(lambda: 1, lambda e: \"x\")\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("from typhon_runtime import try_result"),
            "try_result use must inject its runtime import:\n{out}"
        );
        assert!(
            !out.contains("import Ok, Err, Result"),
            "try_result alone must not inject the Ok/Err/Result family import:\n{out}"
        );
    }

    #[test]
    fn runtime_name_in_lambda_default_injects_import() {
        // A runtime name referenced ONLY inside a lambda's default-argument
        // value (`lambda x=Ok(1): x`) must still trigger the import injection —
        // the walker inspects parameter defaults, not just the lambda body.
        let src = "def f() -> object:\n    let g = lambda x=Ok(1): x\n    return g\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("from typhon_runtime import Ok, Err, Result"),
            "a result name in a lambda default must inject the runtime import:\n{out}"
        );
    }

    #[test]
    fn non_class_statements_pass_through() {
        let src = "x: int = 1\n\ndef f() -> None:\n    pass\n";
        let out = parse_and_desugar(src);
        assert!(!out.contains("dataclass"), "output:\n{out}");
    }

    #[test]
    fn asyncio_injected_for_qualified_use_inside_match_arm() {
        // `gather:` lowers to `async with asyncio.TaskGroup()`; when it sits
        // inside a `case` arm the import injection skipped it because the
        // asyncio-usage walker didn't descend into match statements, so the
        // emitted module raised NameError at runtime (kilnlog #3).
        let src = "async def both(flag: bool) -> int:\n    \
                   match flag:\n        \
                   case True:\n            \
                   async with asyncio.TaskGroup() as tg:\n                \
                   t = tg.create_task(one())\n            \
                   return t.result()\n        \
                   case False:\n            \
                   return 0\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("import asyncio"),
            "asyncio use inside a match arm must inject the import:\n{out}"
        );
    }

    #[test]
    fn asyncio_injected_for_qualified_use_inside_except_handler() {
        // Same omission affected `try` handlers (the `Try` arm only walked
        // body/orelse/finally) — descend into handlers too.
        let src = "async def run() -> int:\n    \
                   try:\n        \
                   return await one()\n    \
                   except ValueError:\n        \
                   async with asyncio.TaskGroup() as tg:\n            \
                   t = tg.create_task(two())\n        \
                   return t.result()\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("import asyncio"),
            "asyncio use inside an except handler must inject the import:\n{out}"
        );
    }

    #[test]
    fn traceback_remap_injects_install_into_main_guard() {
        let src = "def main() -> None:\n    pass\n\nif __name__ == \"__main__\":\n    main()\n";
        let module = tyc_syntax::parse_module(src)
            .expect("parse failed")
            .into_syntax();
        let out = desugar_module_with(
            &module,
            DesugarOptions {
                traceback_remap: true,
                ..Default::default()
            },
        );
        let emitted = emit(&out.module);
        assert!(
            emitted
                .contains("from typhon_runtime.traceback import install as __typhon_install_tb__"),
            "the installer import must be injected:\n{emitted}"
        );
        assert!(
            out.needs_typhon_runtime,
            "the injection must flag the typhon_runtime package for emission"
        );
        // The install call must live INSIDE the `__main__` guard so library
        // imports never trip it.
        let guard = emitted.find("__main__").expect("guard present");
        let call = emitted
            .find("__typhon_install_tb__()")
            .expect("install call present");
        assert!(
            call > guard,
            "install must be inside the __main__ block:\n{emitted}"
        );
    }

    #[test]
    fn traceback_remap_off_by_default_injects_nothing() {
        let src = "def main() -> None:\n    pass\n\nif __name__ == \"__main__\":\n    main()\n";
        let module = tyc_syntax::parse_module(src)
            .expect("parse failed")
            .into_syntax();
        let out = desugar_module_with(&module, DesugarOptions::default());
        let emitted = emit(&out.module);
        assert!(
            !emitted.contains("__typhon_install_tb__"),
            "the default (off) must not inject the installer:\n{emitted}"
        );
        assert!(
            !out.needs_typhon_runtime,
            "a runtime-free entry must stay dependency-free"
        );
    }

    #[test]
    fn multiple_classes_one_import() {
        let src = "class A:\n    x: int\n\nclass B:\n    y: str\n";
        let out = parse_and_desugar(src);
        assert_eq!(
            out.matches("import dataclasses").count(),
            1,
            "output:\n{out}"
        );
        assert_eq!(
            out.matches("@dataclasses.dataclass(slots=True)").count(),
            2,
            "output:\n{out}"
        );
    }

    #[test]
    fn class_inside_function_is_desugared() {
        let src = "def make_point():\n    class Point:\n        x: int\n    return Point\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("@dataclasses.dataclass(slots=True)"),
            "output:\n{out}"
        );
        assert!(out.contains("import dataclasses"), "output:\n{out}");
    }

    #[test]
    fn nested_class_inside_class_is_desugared() {
        let src = "class Outer:\n    x: int\n    class Inner:\n        y: int\n";
        let out = parse_and_desugar(src);
        assert_eq!(
            out.matches("@dataclasses.dataclass(slots=True)").count(),
            2,
            "output:\n{out}"
        );
    }

    #[test]
    fn import_inserted_after_future_imports() {
        let src = "from __future__ import annotations\n\nclass Point:\n    x: int\n";
        let out = parse_and_desugar(src);
        let future_pos = out.find("from __future__").expect("future import missing");
        let import_pos = out
            .find("import dataclasses")
            .expect("dataclasses import missing");
        assert!(
            future_pos < import_pos,
            "from __future__ must precede import dataclasses\noutput:\n{out}"
        );
    }

    #[test]
    fn import_inserted_after_docstring_and_future_imports() {
        let src = "\"\"\"Module docstring.\"\"\"\nfrom __future__ import annotations\n\nclass Point:\n    x: int\n";
        let out = parse_and_desugar(src);
        let doc_pos = out.find("Module docstring").expect("docstring missing");
        let future_pos = out.find("from __future__").expect("future import missing");
        let import_pos = out
            .find("import dataclasses")
            .expect("dataclasses import missing");
        assert!(doc_pos < future_pos, "output:\n{out}");
        assert!(future_pos < import_pos, "output:\n{out}");
    }

    // ── Result import injection ───────────────────────────────────────────────

    #[test]
    fn ok_call_injects_typhon_runtime_import() {
        let src = "def f() -> None:\n    x = Ok(1)\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("from typhon_runtime import"),
            "expected typhon_runtime import; output:\n{out}"
        );
        assert!(out.contains("Ok"), "output:\n{out}");
    }

    #[test]
    fn err_call_injects_typhon_runtime_import() {
        let src = "def f() -> None:\n    x = Err(\"boom\")\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("from typhon_runtime import"),
            "expected typhon_runtime import; output:\n{out}"
        );
    }

    #[test]
    fn result_annotation_injects_typhon_runtime_import() {
        let src = "def f() -> Result[int, str]:\n    return Ok(1)\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("from typhon_runtime import"),
            "expected typhon_runtime import; output:\n{out}"
        );
    }

    #[test]
    fn no_result_usage_no_import_injection() {
        let src = "x: int = 1\n";
        let out = parse_and_desugar(src);
        assert!(
            !out.contains("typhon_runtime"),
            "unexpected typhon_runtime import; output:\n{out}"
        );
    }

    #[test]
    fn existing_typhon_runtime_import_not_duplicated() {
        let src = "from typhon_runtime import Ok, Err, Result\n\ndef f() -> None:\n    x = Ok(1)\n";
        let out = parse_and_desugar(src);
        assert_eq!(
            out.matches("from typhon_runtime import").count(),
            1,
            "should not duplicate existing import; output:\n{out}"
        );
    }

    #[test]
    fn needs_typhon_runtime_flag_set_when_result_used() {
        let src = "def f() -> None:\n    x = Ok(1)\n";
        let parsed = tyc_syntax::parse_module(src).expect("parse failed");
        let module = parsed.into_syntax();
        let output = desugar_module(&module);
        assert!(
            output.needs_typhon_runtime,
            "flag should be true when Ok is used"
        );
    }

    #[test]
    fn needs_typhon_runtime_flag_clear_when_result_not_used() {
        let src = "x: int = 1\n";
        let parsed = tyc_syntax::parse_module(src).expect("parse failed");
        let module = parsed.into_syntax();
        let output = desugar_module(&module);
        assert!(
            !output.needs_typhon_runtime,
            "flag should be false when Result not used"
        );
    }

    #[test]
    fn runtime_import_inserted_after_docstring() {
        let src = "\"\"\"Module doc.\"\"\"\ndef f() -> None:\n    x = Ok(1)\n";
        let out = parse_and_desugar(src);
        let doc_pos = out.find("Module doc").expect("docstring missing");
        let import_pos = out
            .find("from typhon_runtime")
            .expect("runtime import missing");
        assert!(
            doc_pos < import_pos,
            "runtime import must follow docstring\noutput:\n{out}"
        );
    }

    // ── Result detection: extended statement coverage ────────────────────────

    #[test]
    fn ok_inside_while_loop_detected() {
        let src = "while True:\n    x = Ok(1)\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("from typhon_runtime import"), "output:\n{out}");
    }

    #[test]
    fn ok_inside_for_loop_detected() {
        let src = "for i in range(3):\n    x = Ok(i)\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("from typhon_runtime import"), "output:\n{out}");
    }

    #[test]
    fn ok_inside_with_block_detected() {
        let src = "with open('x') as f:\n    r = Ok(f.read())\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("from typhon_runtime import"), "output:\n{out}");
    }

    #[test]
    fn ok_inside_try_block_detected() {
        let src = "try:\n    r = Ok(1)\nexcept Exception:\n    pass\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("from typhon_runtime import"), "output:\n{out}");
    }

    #[test]
    fn ok_inside_match_case_detected() {
        let src = "match x:\n    case 1:\n        r = Ok(1)\n    case _:\n        r = Err('no')\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("from typhon_runtime import"), "output:\n{out}");
    }

    #[test]
    fn result_in_type_alias_detected() {
        let src = "type MyResult = Result[int, str]\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("from typhon_runtime import"), "output:\n{out}");
    }

    #[test]
    fn result_in_param_annotation_detected() {
        let src = "def f(r: Result[int, str]) -> None:\n    pass\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("from typhon_runtime import"), "output:\n{out}");
    }

    #[test]
    fn result_in_if_test_detected() {
        let src = "if isinstance(x, Result):\n    pass\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("from typhon_runtime import"), "output:\n{out}");
    }

    // ── Result detection: extended expression coverage ───────────────────────

    #[test]
    fn ok_inside_list_literal_detected() {
        let src = "x = [Ok(1), Err('no')]\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("from typhon_runtime import"), "output:\n{out}");
    }

    #[test]
    fn ok_inside_dict_literal_detected() {
        let src = "x = {'a': Ok(1)}\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("from typhon_runtime import"), "output:\n{out}");
    }

    #[test]
    fn ok_inside_ifexp_detected() {
        let src = "x = Ok(1) if cond else Err('no')\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("from typhon_runtime import"), "output:\n{out}");
    }

    #[test]
    fn ok_inside_lambda_detected() {
        let src = "f = lambda x: Ok(x)\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("from typhon_runtime import"), "output:\n{out}");
    }

    #[test]
    fn ok_inside_listcomp_detected() {
        let src = "xs = [Ok(i) for i in range(3)]\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("from typhon_runtime import"), "output:\n{out}");
    }

    #[test]
    fn ok_inside_keyword_arg_detected() {
        // The previous detection missed `keywords`; now it must catch this.
        let src = "make(value=Ok(1))\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("from typhon_runtime import"), "output:\n{out}");
    }

    #[test]
    fn ok_inside_fstring_detected() {
        let src = "msg = f'{Ok(1)}'\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("from typhon_runtime import"), "output:\n{out}");
    }

    // ── Import detection edge cases ──────────────────────────────────────────

    #[test]
    fn partial_from_import_still_injects_full_import() {
        // User imported only `Ok` — the emitted module also uses `Result`,
        // so injection must still run to bring `Err`/`Result` into scope.
        let src =
            "from typhon_runtime import Ok\n\ndef f() -> Result[int, str]:\n    return Ok(1)\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("from typhon_runtime import Ok, Err, Result"),
            "expected full injection alongside the partial existing import; output:\n{out}"
        );
    }

    #[test]
    fn full_from_import_suppresses_injection() {
        // Existing import covers all three names — no need to inject ours.
        let src = "from typhon_runtime import Ok, Err, Result\n\ndef f() -> None:\n    x = Ok(1)\n";
        let out = parse_and_desugar(src);
        assert_eq!(
            out.matches("from typhon_runtime import").count(),
            1,
            "should not duplicate the existing complete import; output:\n{out}"
        );
    }

    #[test]
    fn bare_import_typhon_runtime_sets_needs_flag() {
        // `import typhon_runtime` plus qualified `typhon_runtime.Ok(...)` does
        // not bring Ok/Err/Result into scope, but the build still needs to
        // emit the runtime file.
        let src = "import typhon_runtime\nx = typhon_runtime.Ok(1)\n";
        let parsed = tyc_syntax::parse_module(src).expect("parse failed");
        let module = parsed.into_syntax();
        let output = desugar_module(&module);
        assert!(
            output.needs_typhon_runtime,
            "bare `import typhon_runtime` must set needs_typhon_runtime"
        );
    }

    // ── impl block desugaring ─────────────────────────────────────────────────

    #[test]
    fn collect_sealed_union_aliases_preserves_source_order() {
        // PR #129 gemini review: variants must come out left-to-right,
        // matching the source `type T = A | B | C` ordering. The
        // earlier stack-push order yielded `[C, B, A]`, which silently
        // worked because downstream consumers used the set, not the
        // order — but contradicted the docstring.
        let src = "\
class A:
    pass
class B:
    pass
class C:
    pass
type T = A | B | C
";
        // Parse via the desugar entry point so we get a real Stmt list.
        let module = tyc_syntax::parse_module(src)
            .expect("parse failed")
            .into_syntax();
        let aliases = collect_sealed_union_aliases(&module.body);
        let order = aliases.get("T").expect("T alias should be collected");
        assert_eq!(order, &["A".to_owned(), "B".to_owned(), "C".to_owned()]);
    }

    #[test]
    fn impl_block_on_sealed_union_distributes_methods() {
        // R2-3: `impl Event:` where `Event = A | B` must replicate the
        // methods on each variant class so `event.kind()` resolves at
        // every call site regardless of the runtime variant.
        let src = "\
class A:
    x: int

class B:
    y: str

type Event = A | B

class __typhon_impl_Event(object):
    def kind():
        return 'a'
";
        let out = parse_and_desugar(src);
        assert_eq!(
            out.matches("def kind(self):").count(),
            2,
            "kind must be replicated on both A and B; output:\n{out}"
        );
        assert!(
            !out.contains("__typhon_impl_"),
            "impl stub must be removed even for union impls; output:\n{out}"
        );
    }

    #[test]
    fn impl_block_methods_merged_into_target_class() {
        // Simulates what the preprocessor produces from `impl User:`.
        let src = "class User:\n    name: str\n\nclass __typhon_impl_User(object):\n    def greet():\n        return 'Hello'\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("def greet(self):"),
            "self must be injected; output:\n{out}"
        );
        assert!(
            !out.contains("__typhon_impl_"),
            "impl stub must be removed; output:\n{out}"
        );
    }

    #[test]
    fn impl_block_stub_not_decorated_as_dataclass() {
        let src = "class User:\n    name: str\n\nclass __typhon_impl_User(object):\n    def greet():\n        pass\n";
        let out = parse_and_desugar(src);
        // Exactly one @dataclasses.dataclass — on User, not on the impl stub.
        assert_eq!(
            out.matches("@dataclasses.dataclass").count(),
            1,
            "only the target class must be decorated; output:\n{out}"
        );
    }

    #[test]
    fn impl_block_multiple_methods_all_get_self() {
        let src = "class Point:\n    x: int\n    y: int\n\nclass __typhon_impl_Point(object):\n    def translate():\n        pass\n    def scale():\n        pass\n";
        let out = parse_and_desugar(src);
        assert_eq!(
            out.matches("def translate(self):").count(),
            1,
            "output:\n{out}"
        );
        assert_eq!(out.matches("def scale(self):").count(), 1, "output:\n{out}");
    }

    #[test]
    fn typed_dict_literal_lowers_to_constructor_call() {
        // `let u: User = {"id": 1, "name": "ada"}` — the checker accepts the
        // TypedDict-style match (v0.3.0); the emit must construct the class,
        // not keep the raw dict (which crashed on first attribute access).
        let src = "class User:\n    id: int\n    name: str\n\ndef f() -> None:\n    u: User = {\"id\": 1, \"name\": \"ada\"}\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("User(id=1, name=") || out.contains("User(id=1, name="),
            "dict literal must lower to a ctor call; output:\n{out}"
        );
    }

    #[test]
    fn typed_dict_literal_nested_class_field_recurses() {
        let src = "class Address:\n    city: str\n\nclass Person:\n    name: str\n    address: Address\n\ndef f() -> None:\n    p: Person = {\"name\": \"ada\", \"address\": {\"city\": \"London\"}}\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("address=Address(city="),
            "nested dict against a class-typed field must recurse; output:\n{out}"
        );
    }

    #[test]
    fn untyped_dict_annotation_keeps_dict_literal() {
        // `dict[str, int]`-annotated literals must stay dicts.
        let src = "def f() -> None:\n    d: dict = {\"a\": 1}\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("{\"a\": 1}") || out.contains("{'a': 1}"),
            "plain dict annotations must be untouched; output:\n{out}"
        );
    }

    #[test]
    fn dict_literal_with_non_identifier_key_untouched() {
        // A key that can't be a kwarg (`"not-an-ident"`) must abort the
        // rewrite even when the annotation names a local class.
        let src =
            "class User:\n    id: int\n\ndef f() -> None:\n    u: User = {\"not-an-ident\": 1}\n";
        let out = parse_and_desugar(src);
        assert!(
            !out.contains("User(") || out.contains("class User"),
            "non-identifier keys must leave the dict untouched; output:\n{out}"
        );
        assert!(out.contains("not-an-ident"), "output:\n{out}");
    }

    #[test]
    fn cross_module_extend_lowers_to_attribute_patch() {
        // `extend Record:` where `Record` is imported (not declared here)
        // must lower to a module-level def + class-attribute assignment —
        // previously the methods were silently dropped (runtime
        // AttributeError after a clean build).
        let src = "from store.records import Record\n\nclass __typhon_impl_Record(object):\n    def label(self):\n        return self.name\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("def __typhon_extend_Record__label(self):"),
            "foreign extend must synthesise a module-level fn; output:\n{out}"
        );
        assert!(
            out.contains("Record.label = __typhon_extend_Record__label"),
            "foreign extend must patch the class attribute; output:\n{out}"
        );
        assert!(
            !out.contains("__typhon_impl_"),
            "impl stub must be removed; output:\n{out}"
        );
    }

    #[test]
    fn cross_module_extend_patch_lands_after_import() {
        let src = "from store.records import Record\n\nclass __typhon_impl_Record(object):\n    def label(self):\n        return self.name\n";
        let out = parse_and_desugar(src);
        let import_pos = out.find("from store.records import Record").unwrap();
        let patch_pos = out.find("Record.label =").unwrap();
        assert!(
            import_pos < patch_pos,
            "patch must execute after the import binds the class; output:\n{out}"
        );
    }

    #[test]
    fn local_extend_still_merges_into_class_body() {
        // A same-module target keeps the body-merge lowering (no patches).
        let src = "class Thing:\n    name: str\n\nclass __typhon_impl_Thing(object):\n    def shout(self):\n        return self.name\n";
        let out = parse_and_desugar(src);
        assert!(
            !out.contains("__typhon_extend_"),
            "local targets must not use the patch form; output:\n{out}"
        );
        assert!(out.contains("def shout(self):"), "output:\n{out}");
    }

    #[test]
    fn impl_block_existing_self_not_duplicated() {
        // If the method already has `self` as first param, do not add a second one.
        let src = "class User:\n    name: str\n\nclass __typhon_impl_User(object):\n    def greet(self):\n        pass\n";
        let out = parse_and_desugar(src);
        // `def greet(self):` must appear exactly once, not `def greet(self, self):`.
        assert_eq!(out.matches("def greet(self):").count(), 1, "output:\n{out}");
    }

    #[test]
    fn impl_block_async_method_gets_self() {
        let src = "class Fetcher:\n    url: str\n\nclass __typhon_impl_Fetcher(object):\n    async def fetch():\n        pass\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("async def fetch(self):"),
            "async method must get self; output:\n{out}"
        );
        assert!(
            !out.contains("__typhon_impl_"),
            "impl stub must be removed; output:\n{out}"
        );
    }

    #[test]
    fn impl_block_staticmethod_no_self_injected() {
        let src = "class Util:\n    x: int\n\nclass __typhon_impl_Util(object):\n    @staticmethod\n    def helper():\n        pass\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("def helper():"),
            "staticmethod must not get self; output:\n{out}"
        );
        assert!(!out.contains("def helper(self)"), "output:\n{out}");
    }

    #[test]
    fn impl_block_classmethod_gets_cls() {
        let src = "class Repo:\n    name: str\n\nclass __typhon_impl_Repo(object):\n    @classmethod\n    def create():\n        return cls()\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("def create(cls):"),
            "classmethod must get cls; output:\n{out}"
        );
        assert!(!out.contains("def create(self)"), "output:\n{out}");
    }

    #[test]
    fn star_from_typhon_runtime_suppresses_injection() {
        let src = "from typhon_runtime import *\n\ndef f() -> None:\n    x = Ok(1)\n";
        let out = parse_and_desugar(src);
        assert_eq!(
            out.matches("from typhon_runtime import").count(),
            1,
            "star-import covers all names; should not duplicate; output:\n{out}"
        );
    }

    // ── Pydantic model desugaring ─────────────────────────────────────────────

    #[test]
    fn basemodel_class_gets_pydantic_import() {
        // Simulates what the preprocessor produces from `model ApiUser:`.
        let src = "class ApiUser(BaseModel):\n    id: int\n    email: str\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("from pydantic import BaseModel"),
            "output:\n{out}"
        );
    }

    #[test]
    fn basemodel_class_does_not_get_dataclass_decorator() {
        let src = "class ApiUser(BaseModel):\n    id: int\n";
        let out = parse_and_desugar(src);
        assert!(
            !out.contains("@dataclasses.dataclass"),
            "BaseModel classes must not get the dataclass decorator\noutput:\n{out}"
        );
        assert!(
            !out.contains("import dataclasses"),
            "no dataclasses import for BaseModel classes\noutput:\n{out}"
        );
    }

    #[test]
    fn existing_pydantic_import_not_duplicated() {
        let src = "from pydantic import BaseModel\n\nclass User(BaseModel):\n    id: int\n";
        let out = parse_and_desugar(src);
        assert_eq!(
            out.matches("from pydantic import BaseModel").count(),
            1,
            "must not duplicate existing pydantic import\noutput:\n{out}"
        );
    }

    #[test]
    fn plain_class_and_model_class_in_same_module() {
        let src = "class Point:\n    x: int\n\nclass User(BaseModel):\n    id: int\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("@dataclasses.dataclass(slots=True)"),
            "Point needs dataclass\noutput:\n{out}"
        );
        assert!(
            out.contains("from pydantic import BaseModel"),
            "User needs pydantic\noutput:\n{out}"
        );
        assert!(
            !out.contains("@dataclasses.dataclass")
                || out.matches("@dataclasses.dataclass").count() == 1,
            "User must NOT get dataclass decorator\noutput:\n{out}"
        );
    }

    // ── Pydantic ConfigDict(extra="forbid") injection ─────────────────────────

    #[test]
    fn model_class_gets_model_config_forbid() {
        let src = "class ApiUser(BaseModel):\n    id: int\n    email: str\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("model_config = ConfigDict(extra=\"forbid\")"),
            "BaseModel class must get model_config = ConfigDict(extra=\"forbid\")\noutput:\n{out}"
        );
    }

    #[test]
    fn model_class_import_includes_config_dict() {
        let src = "class ApiUser(BaseModel):\n    id: int\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("ConfigDict"),
            "injected pydantic import must include ConfigDict\noutput:\n{out}"
        );
        assert!(
            out.contains("from pydantic import"),
            "pydantic import must be present\noutput:\n{out}"
        );
    }

    #[test]
    fn model_class_existing_model_config_not_duplicated() {
        let src = "class ApiUser(BaseModel):\n    model_config = ConfigDict(extra=\"allow\")\n    id: int\n";
        let out = parse_and_desugar(src);
        assert_eq!(
            out.matches("model_config").count(),
            1,
            "existing model_config must not be duplicated\noutput:\n{out}"
        );
        assert!(
            out.contains("extra=\"allow\""),
            "user-defined model_config must be preserved\noutput:\n{out}"
        );
    }

    #[test]
    fn model_config_is_first_body_statement() {
        let src = "class ApiUser(BaseModel):\n    id: int\n    name: str\n";
        let out = parse_and_desugar(src);
        let model_config_pos = out
            .find("model_config")
            .expect("model_config must be present");
        let id_pos = out.find("id: int").expect("id field must be present");
        assert!(
            model_config_pos < id_pos,
            "model_config must precede field declarations\noutput:\n{out}"
        );
    }

    #[test]
    fn model_config_injected_after_class_docstring() {
        // The docstring must remain the first statement; model_config follows it.
        let src = "class ApiUser(BaseModel):\n    \"\"\"An API user.\"\"\"\n    id: int\n";
        let out = parse_and_desugar(src);
        let doc_pos = out.find("An API user").expect("docstring missing");
        let config_pos = out.find("model_config").expect("model_config missing");
        assert!(
            doc_pos < config_pos,
            "docstring must precede model_config\noutput:\n{out}"
        );
    }

    #[test]
    fn config_dict_injected_when_basemodel_already_imported() {
        // Regression: if the user already has `from pydantic import BaseModel`,
        // the desugarer must still ensure `ConfigDict` is in scope.
        let src = "from pydantic import BaseModel\n\nclass ApiUser(BaseModel):\n    id: int\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("ConfigDict"),
            "ConfigDict must be imported even when BaseModel is already present\noutput:\n{out}"
        );
        assert!(
            out.contains("model_config = ConfigDict(extra=\"forbid\")"),
            "model_config must still be injected\noutput:\n{out}"
        );
    }

    #[test]
    fn config_dict_not_duplicated_when_already_imported() {
        let src = "from pydantic import BaseModel, ConfigDict\n\nclass ApiUser(BaseModel):\n    id: int\n";
        let out = parse_and_desugar(src);
        assert_eq!(
            out.matches("ConfigDict").count(),
            // "ConfigDict" appears in the import and in the model_config assignment
            2,
            "ConfigDict must not be imported a second time\noutput:\n{out}"
        );
    }

    // ── [emit] model-extra tests ──────────────────────────────────────────

    fn parse_and_desugar_with_model_extra(src: &str, extra: &str) -> String {
        let module = tyc_syntax::parse_module(src)
            .expect("parse failed")
            .into_syntax();
        let out = desugar_module_with(
            &module,
            DesugarOptions {
                model_extra: extra.into(),
                ..Default::default()
            },
        );
        emit(&out.module)
    }

    #[test]
    fn model_extra_forbid_is_default() {
        // model-extra defaults to "forbid"; the existing test already covers
        // this via parse_and_desugar; verify via explicit option too.
        let src = "class ApiUser(BaseModel):\n    id: int\n";
        let out = parse_and_desugar_with_model_extra(src, "forbid");
        assert!(
            out.contains("model_config = ConfigDict(extra=\"forbid\")"),
            "model-extra=\"forbid\" must emit extra=\"forbid\"\noutput:\n{out}"
        );
    }

    #[test]
    fn model_extra_allow_emits_allow() {
        let src = "class ApiUser(BaseModel):\n    id: int\n";
        let out = parse_and_desugar_with_model_extra(src, "allow");
        assert!(
            out.contains("model_config = ConfigDict(extra=\"allow\")"),
            "model-extra=\"allow\" must emit extra=\"allow\"\noutput:\n{out}"
        );
        assert!(
            !out.contains("extra=\"forbid\""),
            "must not emit \"forbid\" when model-extra=\"allow\"\noutput:\n{out}"
        );
    }

    #[test]
    fn model_extra_ignore_emits_ignore() {
        let src = "class ApiUser(BaseModel):\n    id: int\n";
        let out = parse_and_desugar_with_model_extra(src, "ignore");
        assert!(
            out.contains("model_config = ConfigDict(extra=\"ignore\")"),
            "model-extra=\"ignore\" must emit extra=\"ignore\"\noutput:\n{out}"
        );
    }

    #[test]
    fn model_extra_does_not_affect_non_model_classes() {
        // Plain dataclass must not get a model_config stmt regardless of model-extra.
        let src = "class Point:\n    x: float\n    y: float\n";
        let out = parse_and_desugar_with_model_extra(src, "allow");
        assert!(
            !out.contains("model_config"),
            "non-model class must not get model_config with model-extra=\"allow\"\noutput:\n{out}"
        );
    }

    #[test]
    fn model_extra_respects_existing_user_model_config() {
        // If the user already wrote model_config, the desugar pass must NOT
        // inject a second one — even if model-extra differs.
        let src =
            "class ApiUser(BaseModel):\n    model_config = ConfigDict(extra=\"allow\")\n    id: int\n";
        let out = parse_and_desugar_with_model_extra(src, "ignore");
        assert_eq!(
            out.matches("model_config").count(),
            1,
            "existing model_config must not be duplicated\noutput:\n{out}"
        );
        assert!(
            out.contains("extra=\"allow\""),
            "user-defined model_config value must be preserved\noutput:\n{out}"
        );
    }

    #[test]
    fn typed_dict_skips_dataclass_decorator() {
        let src = "from typing import TypedDict\nclass U(TypedDict):\n    id: int\n    name: str\n";
        let out = parse_and_desugar(src);
        assert!(
            !out.contains("@dataclasses.dataclass"),
            "TypedDict must not get @dataclasses.dataclass decorator: {out}"
        );
    }

    #[test]
    fn typed_dict_qualified_skips_dataclass_decorator() {
        // `typing.TypedDict` (qualified) must also be detected.
        let src = "import typing\nclass U(typing.TypedDict):\n    id: int\n";
        let out = parse_and_desugar(src);
        assert!(
            !out.contains("@dataclasses.dataclass"),
            "typing.TypedDict must not get @dataclasses.dataclass decorator: {out}"
        );
    }

    #[test]
    fn typed_dict_extensions_qualified_skips_dataclass_decorator() {
        // `typing_extensions.TypedDict` must also be detected.
        let src = "import typing_extensions\nclass U(typing_extensions.TypedDict):\n    id: int\n";
        let out = parse_and_desugar(src);
        assert!(
            !out.contains("@dataclasses.dataclass"),
            "typing_extensions.TypedDict must not get @dataclasses.dataclass decorator: {out}"
        );
    }
}
