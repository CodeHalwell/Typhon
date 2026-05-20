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
    Alias, Arguments, AtomicNodeIndex, Decorator, ExceptHandler, Expr, ExprAttribute,
    ExprBooleanLiteral, ExprCall, ExprContext, ExprName, ExprStringLiteral, Identifier, Keyword,
    ModModule, Parameter, ParameterWithDefault, Parameters, Stmt, StmtAssign, StmtImport,
    StmtImportFrom, StringLiteral, StringLiteralFlags, StringLiteralValue, WithItem,
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
#[derive(Debug, Default, Clone)]
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
    /// (`[strictness] skip-decoration-bases`).
    pub skip_decoration_bases: Vec<String>,
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
pub fn desugar_module(module: &ModModule) -> DesugarOutput {
    desugar_module_with(module, DesugarOptions::default())
}

/// Same as [`desugar_module`] but accepts caller-supplied [`DesugarOptions`].
///
/// Used by `tyc build` to thread purity-analysis results (which top-level
/// functions opted into `@memo` and therefore need an injected
/// `@functools.cache` decorator) through to the desugar pass.
pub fn desugar_module_with(module: &ModModule, options: DesugarOptions) -> DesugarOutput {
    let desugared_mod = desugar_mod_module_with(module, &options);

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
    let needs_typhon_runtime = has_result_usage || has_any_runtime_import || has_runtime_qualified;
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

    // `gather:` lowers to `asyncio.TaskGroup` and best-effort to
    // `asyncio.gather(...)` — ensure `import asyncio` is in scope.
    let needs_asyncio = stmts_use_asyncio_qualified(&desugared_mod.body);
    let inject_asyncio = needs_asyncio && !has_asyncio_import(&desugared_mod.body);

    // `go` and `lazy` lower to `typhon_runtime.tasks.spawn(...)` /
    // `typhon_runtime.lazy.…` — ensure a bare `import typhon_runtime`
    // is in scope when the user hasn't already arranged one.
    let inject_runtime_module =
        has_runtime_qualified && !has_bare_typhon_runtime_import(&desugared_mod.body);

    let mut body = desugared_mod.body;
    let insert_at = import_insert_pos(&body);

    // Insert imports in reverse order so later `insert_at` calls don't
    // shift indices of earlier insertions.
    if inject_runtime_module {
        body.insert(insert_at, make_bare_typhon_runtime_import());
    }
    if inject_asyncio {
        body.insert(insert_at, make_asyncio_import());
    }
    if inject_protocol {
        body.insert(insert_at, make_protocol_import());
    }
    if inject_result_import {
        body.insert(insert_at, make_typhon_runtime_import());
    }
    // Emit the fewest imports possible: combine into one statement when
    // both are needed, otherwise emit just the missing one.
    if inject_basemodel && inject_config_dict {
        body.insert(insert_at, make_pydantic_basemodel_import()); // includes both
    } else if inject_basemodel {
        body.insert(insert_at, make_pydantic_basemodel_only_import());
    } else if inject_config_dict {
        body.insert(insert_at, make_config_dict_only_import());
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

// ── Result detection ─────────────────────────────────────────────────────────

/// Return `true` if any statement in `stmts` (or its nested bodies) references
/// the identifiers `Ok`, `Err`, or `Result`.
fn stmts_use_result_names(stmts: &[Stmt]) -> bool {
    stmts.iter().any(stmt_uses_result_names)
}

fn stmt_uses_result_names(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::FunctionDef(f) => {
            f.returns
                .as_ref()
                .is_some_and(|r| expr_uses_result_names(r))
                || parameters_use_result_names(&f.parameters)
                || f.decorator_list.iter().any(decorator_uses_result_names)
                || stmts_use_result_names(&f.body)
        }
        Stmt::ClassDef(c) => {
            c.decorator_list.iter().any(decorator_uses_result_names)
                || c.bases().iter().any(expr_uses_result_names)
                || c.keywords()
                    .iter()
                    .any(|k| expr_uses_result_names(&k.value))
                || stmts_use_result_names(&c.body)
        }
        Stmt::AnnAssign(a) => {
            expr_uses_result_names(&a.annotation)
                || a.value.as_ref().is_some_and(|v| expr_uses_result_names(v))
                || expr_uses_result_names(&a.target)
        }
        Stmt::Assign(a) => {
            expr_uses_result_names(&a.value) || a.targets.iter().any(expr_uses_result_names)
        }
        Stmt::AugAssign(a) => expr_uses_result_names(&a.target) || expr_uses_result_names(&a.value),
        Stmt::Return(r) => r.value.as_ref().is_some_and(|v| expr_uses_result_names(v)),
        Stmt::Expr(e) => expr_uses_result_names(&e.value),
        Stmt::If(i) => {
            expr_uses_result_names(&i.test)
                || stmts_use_result_names(&i.body)
                || i.elif_else_clauses.iter().any(|c| {
                    c.test.as_ref().is_some_and(expr_uses_result_names)
                        || stmts_use_result_names(&c.body)
                })
        }
        Stmt::While(w) => {
            expr_uses_result_names(&w.test)
                || stmts_use_result_names(&w.body)
                || stmts_use_result_names(&w.orelse)
        }
        Stmt::For(f) => {
            expr_uses_result_names(&f.target)
                || expr_uses_result_names(&f.iter)
                || stmts_use_result_names(&f.body)
                || stmts_use_result_names(&f.orelse)
        }
        Stmt::With(w) => {
            w.items.iter().any(with_item_uses_result_names) || stmts_use_result_names(&w.body)
        }
        Stmt::Try(t) => {
            stmts_use_result_names(&t.body)
                || t.handlers.iter().any(except_handler_uses_result_names)
                || stmts_use_result_names(&t.orelse)
                || stmts_use_result_names(&t.finalbody)
        }
        Stmt::Match(m) => {
            expr_uses_result_names(&m.subject)
                || m.cases.iter().any(|case| {
                    case.guard
                        .as_ref()
                        .is_some_and(|g| expr_uses_result_names(g))
                        || stmts_use_result_names(&case.body)
                })
        }
        Stmt::Raise(r) => {
            r.exc.as_ref().is_some_and(|e| expr_uses_result_names(e))
                || r.cause.as_ref().is_some_and(|c| expr_uses_result_names(c))
        }
        Stmt::Assert(a) => {
            expr_uses_result_names(&a.test)
                || a.msg.as_ref().is_some_and(|m| expr_uses_result_names(m))
        }
        Stmt::Delete(d) => d.targets.iter().any(expr_uses_result_names),
        Stmt::TypeAlias(t) => expr_uses_result_names(&t.name) || expr_uses_result_names(&t.value),
        _ => false,
    }
}

fn parameters_use_result_names(params: &Parameters) -> bool {
    let plain_param_uses = |p: &Parameter| {
        p.annotation
            .as_ref()
            .is_some_and(|a| expr_uses_result_names(a))
    };
    let with_default_uses = |p: &ParameterWithDefault| {
        plain_param_uses(&p.parameter)
            || p.default
                .as_ref()
                .is_some_and(|d| expr_uses_result_names(d))
    };
    params.posonlyargs.iter().any(with_default_uses)
        || params.args.iter().any(with_default_uses)
        || params.kwonlyargs.iter().any(with_default_uses)
        || params.vararg.as_ref().is_some_and(|a| plain_param_uses(a))
        || params.kwarg.as_ref().is_some_and(|a| plain_param_uses(a))
}

fn with_item_uses_result_names(item: &WithItem) -> bool {
    expr_uses_result_names(&item.context_expr)
        || item
            .optional_vars
            .as_ref()
            .is_some_and(|v| expr_uses_result_names(v))
}

fn except_handler_uses_result_names(handler: &ExceptHandler) -> bool {
    let ExceptHandler::ExceptHandler(h) = handler;
    h.type_.as_ref().is_some_and(|t| expr_uses_result_names(t)) || stmts_use_result_names(&h.body)
}

fn decorator_uses_result_names(d: &Decorator) -> bool {
    expr_uses_result_names(&d.expression)
}

fn expr_uses_result_names(expr: &Expr) -> bool {
    match expr {
        Expr::Name(n) => matches!(n.id.as_str(), "Ok" | "Err" | "Result"),
        Expr::Call(c) => {
            expr_uses_result_names(&c.func)
                || c.arguments.args.iter().any(expr_uses_result_names)
                || c.arguments
                    .keywords
                    .iter()
                    .any(|k| expr_uses_result_names(&k.value))
        }
        Expr::Subscript(s) => expr_uses_result_names(&s.value) || expr_uses_result_names(&s.slice),
        Expr::BinOp(b) => expr_uses_result_names(&b.left) || expr_uses_result_names(&b.right),
        Expr::BoolOp(b) => b.values.iter().any(expr_uses_result_names),
        Expr::UnaryOp(u) => expr_uses_result_names(&u.operand),
        Expr::Named(n) => expr_uses_result_names(&n.target) || expr_uses_result_names(&n.value),
        Expr::Compare(c) => {
            expr_uses_result_names(&c.left) || c.comparators.iter().any(expr_uses_result_names)
        }
        Expr::Lambda(l) => expr_uses_result_names(&l.body),
        Expr::If(i) => {
            expr_uses_result_names(&i.test)
                || expr_uses_result_names(&i.body)
                || expr_uses_result_names(&i.orelse)
        }
        Expr::Tuple(t) => t.elts.iter().any(expr_uses_result_names),
        Expr::List(l) => l.elts.iter().any(expr_uses_result_names),
        Expr::Set(s) => s.elts.iter().any(expr_uses_result_names),
        Expr::Dict(d) => d.items.iter().any(|item| {
            item.key.as_ref().is_some_and(expr_uses_result_names)
                || expr_uses_result_names(&item.value)
        }),
        Expr::ListComp(c) => {
            expr_uses_result_names(&c.elt)
                || c.generators.iter().any(comprehension_uses_result_names)
        }
        Expr::SetComp(c) => {
            expr_uses_result_names(&c.elt)
                || c.generators.iter().any(comprehension_uses_result_names)
        }
        Expr::Generator(g) => {
            expr_uses_result_names(&g.elt)
                || g.generators.iter().any(comprehension_uses_result_names)
        }
        Expr::DictComp(d) => {
            d.key.as_ref().is_some_and(|k| expr_uses_result_names(k))
                || expr_uses_result_names(&d.value)
                || d.generators.iter().any(comprehension_uses_result_names)
        }
        Expr::Await(a) => expr_uses_result_names(&a.value),
        Expr::Yield(y) => y.value.as_ref().is_some_and(|v| expr_uses_result_names(v)),
        Expr::YieldFrom(y) => expr_uses_result_names(&y.value),
        Expr::Starred(s) => expr_uses_result_names(&s.value),
        Expr::Slice(s) => {
            s.lower.as_ref().is_some_and(|e| expr_uses_result_names(e))
                || s.upper.as_ref().is_some_and(|e| expr_uses_result_names(e))
                || s.step.as_ref().is_some_and(|e| expr_uses_result_names(e))
        }
        Expr::FString(f) => f.value.elements().any(|el| match el {
            ruff_python_ast::InterpolatedStringElement::Interpolation(i) => {
                expr_uses_result_names(&i.expression)
            }
            ruff_python_ast::InterpolatedStringElement::Literal(_) => false,
        }),
        Expr::Attribute(a) => expr_uses_result_names(&a.value),
        // Leaf nodes that cannot contain Result names: literals, etc.
        _ => false,
    }
}

fn comprehension_uses_result_names(gen: &ruff_python_ast::Comprehension) -> bool {
    expr_uses_result_names(&gen.target)
        || expr_uses_result_names(&gen.iter)
        || gen.ifs.iter().any(expr_uses_result_names)
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
                || stmts_use_asyncio_qualified(&t.orelse)
                || stmts_use_asyncio_qualified(&t.finalbody)
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
    let multi_base_parents = collect_multi_base_parents(&m.body);
    // Classes whose impl-stub body contains a `cached_property` method (or
    // the aliased `_typhon_cached_property` form emitted by `lazy let` in
    // an impl block) need `__dict__` to back the property cache; treat them
    // as multi-base parents so the dataclass decorator drops `slots=True`.
    let mut multi_base_parents = multi_base_parents;
    collect_cached_property_targets_into(&m.body, &mut multi_base_parents);
    let markers = ClassMarkers {
        raw_starts: &options.raw_class_line_starts,
        frozen_starts: &options.frozen_class_line_starts,
        plain_starts: &options.plain_class_line_starts,
        multi_base_parents: &multi_base_parents,
        skip_decoration_bases: &options.skip_decoration_bases,
    };
    let (new_body, transformed_classes) = desugar_stmts(&m.body, markers);

    // Merge `impl` pseudo-classes into their target classes and remove the stubs.
    let (merged_body, _) = merge_impl_blocks(new_body);

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
    let new_body: Vec<Stmt> = body
        .into_iter()
        .map(|stmt| match stmt {
            Stmt::FunctionDef(mut f) => {
                f.decorator_list = strip_purity_decorators(f.decorator_list);
                f.body = strip_purity_decorators_in_body(f.body);
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
                c.body = strip_purity_decorators_in_body(c.body);
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
fn strip_purity_decorators_in_body(body: Vec<Stmt>) -> Vec<Stmt> {
    body.into_iter()
        .map(|stmt| match stmt {
            Stmt::FunctionDef(mut f) => {
                f.decorator_list = strip_purity_decorators(f.decorator_list);
                f.body = strip_purity_decorators_in_body(f.body);
                Stmt::FunctionDef(f)
            }
            Stmt::ClassDef(mut c) => {
                c.body = strip_purity_decorators_in_body(c.body);
                Stmt::ClassDef(c)
            }
            other => other,
        })
        .collect()
}

/// Drop `@pure`, `@pure(...)`, and `@memo` decorators from a function — they
/// are Typhon-only metadata, not actual Python runtime decorators.
fn strip_purity_decorators(decorators: Vec<Decorator>) -> Vec<Decorator> {
    decorators
        .into_iter()
        .filter(|d| !is_purity_marker(&d.expression))
        .collect()
}

fn is_purity_marker(d: &Expr) -> bool {
    match d {
        // `gatherable` lives alongside `pure` / `memo` as a Typhon-internal
        // attestation: the user marks `async def fetch_user(...)` with it
        // to opt the function in as an auto-gather candidate, but the name
        // has no Python runtime form so the emitter must drop it.
        Expr::Name(n) => matches!(n.id.as_str(), "pure" | "memo" | "gatherable"),
        Expr::Call(c) => is_purity_marker(&c.func),
        _ => false,
    }
}

fn has_cache_decorator(decorators: &[Decorator]) -> bool {
    decorators.iter().any(|d| {
        let path = match &d.expression {
            Expr::Name(n) => Some(n.id.as_str().to_owned()),
            Expr::Attribute(a) => {
                if let Expr::Name(n) = a.value.as_ref() {
                    Some(format!("{}.{}", n.id.as_str(), a.attr.as_str()))
                } else {
                    None
                }
            }
            Expr::Call(c) => match c.func.as_ref() {
                Expr::Name(n) => Some(n.id.as_str().to_owned()),
                Expr::Attribute(a) => {
                    if let Expr::Name(n) = a.value.as_ref() {
                        Some(format!("{}.{}", n.id.as_str(), a.attr.as_str()))
                    } else {
                        None
                    }
                }
                _ => None,
            },
            _ => None,
        };
        matches!(
            path.as_deref(),
            Some("cache" | "lru_cache" | "functools.cache" | "functools.lru_cache")
        )
    })
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
    multi_base_parents: &'a std::collections::HashSet<String>,
    /// User-supplied list of base names whose subclasses should skip the
    /// auto `@dataclass` decoration. Matched by last identifier segment
    /// against each base in the class header.
    skip_decoration_bases: &'a [String],
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
                new_class.body.insert(insert_at, make_model_config_stmt());
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
            if is_raw && !is_plain && class_has_any_base(c) && !body_has_init(&new_class.body) {
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
fn collect_cached_property_targets_into(
    body: &[Stmt],
    parents: &mut std::collections::HashSet<String>,
) {
    for stmt in body {
        if let Stmt::ClassDef(c) = stmt {
            let target = c
                .name
                .as_str()
                .strip_prefix("__typhon_impl_")
                .unwrap_or(c.name.as_str())
                .to_owned();
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

fn collect_multi_base_parents(body: &[Stmt]) -> std::collections::HashSet<String> {
    let mut parents: std::collections::HashSet<String> = std::collections::HashSet::new();
    collect_multi_base_parents_into(body, &mut parents);
    parents
}

fn collect_multi_base_parents_into(body: &[Stmt], parents: &mut std::collections::HashSet<String>) {
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
                            parents.insert(n.id.as_str().to_owned());
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

/// Build `model_config = ConfigDict(extra="forbid")`.
fn make_model_config_stmt() -> Stmt {
    let forbid_lit = make_string_literal_expr("forbid");

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
                if let Expr::Name(n) = target.as_ref() {
                    return Some((&n.id, (**annotation).clone(), value.as_deref().cloned()));
                }
            }
            None
        })
        .collect();
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
    // Phase 1: identify impl pseudo-class indices and their target names.
    let impl_indices: Vec<(usize, String)> = body
        .iter()
        .enumerate()
        .filter_map(|(i, stmt)| {
            if let Stmt::ClassDef(c) = stmt {
                if let Some(target) = c.name.as_str().strip_prefix(IMPL_PREFIX) {
                    return Some((i, target.to_owned()));
                }
            }
            None
        })
        .collect();

    if impl_indices.is_empty() {
        return (body, false);
    }

    // Phase 2: collect methods (with `self` injected) into a map keyed by
    // target class name.  Multiple impl blocks for the same class accumulate.
    let impl_index_set: HashSet<usize> = impl_indices.iter().map(|(i, _)| *i).collect();
    let mut impl_methods_map: HashMap<String, Vec<Stmt>> = HashMap::new();
    for (impl_idx, target_name) in &impl_indices {
        if let Stmt::ClassDef(c) = &body[*impl_idx] {
            let methods: Vec<Stmt> = c.body.iter().map(insert_self_param).collect();
            impl_methods_map
                .entry(target_name.clone())
                .or_default()
                .extend(methods);
        }
    }

    // Phase 3: rebuild the body, merging methods into target classes and
    // dropping the impl pseudo-classes.
    let new_body: Vec<Stmt> = body
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !impl_index_set.contains(i))
        .map(|(_, stmt)| {
            if let Stmt::ClassDef(mut c) = stmt {
                let name = c.name.as_str().to_owned();
                if let Some(methods) = impl_methods_map.remove(&name) {
                    c.body.extend(methods);
                }
                Stmt::ClassDef(c)
            } else {
                stmt
            }
        })
        .collect();

    (new_body, true)
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
        assert!(
            emitted.contains("super().__init__()"),
            "expected super() call:\n{emitted}"
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
    fn non_class_statements_pass_through() {
        let src = "x: int = 1\n\ndef f() -> None:\n    pass\n";
        let out = parse_and_desugar(src);
        assert!(!out.contains("dataclass"), "output:\n{out}");
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
