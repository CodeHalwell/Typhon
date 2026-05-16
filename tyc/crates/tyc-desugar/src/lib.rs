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

use rustpython_ast::{
    text_size::TextRange, Alias, Arg, ArgWithDefault, Constant, Expr, ExprCall, ExprConstant,
    ExprContext, ExprName, Identifier, Mod, ModModule, Stmt, StmtAssign, StmtImport,
    StmtImportFrom,
};

// ── public API ───────────────────────────────────────────────────────────────

/// Output of the module desugaring pass.
pub struct DesugarOutput {
    /// The desugared Python-compatible AST.
    pub module: Mod<TextRange>,
    /// Whether the emitted module will import from `typhon_runtime`. When
    /// true, the build command must write `typhon_runtime.py` alongside the
    /// other output files so the generated import can resolve at runtime.
    pub needs_typhon_runtime: bool,
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
pub fn desugar_module(module: &Mod<TextRange>) -> DesugarOutput {
    match module {
        Mod::Module(m) => {
            let desugared_mod = desugar_mod_module(m);

            let has_result_usage = stmts_use_result_names(&m.body);
            let has_any_runtime_import = has_any_typhon_runtime_import(&m.body);
            let import_covers_all = typhon_runtime_import_covers_all(&m.body);
            // The build must write `typhon_runtime.py` whenever the emitted
            // module will reference it — either because we detected an Ok/
            // Err/Result name, or because the user already imported the
            // runtime explicitly (bare or `from ... import`).
            let needs_typhon_runtime = has_result_usage || has_any_runtime_import;
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

            let mut body = desugared_mod.body;
            let insert_at = import_insert_pos(&body);

            // Insert imports in reverse order so later `insert_at` calls don't
            // shift indices of earlier insertions.
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
                module: Mod::Module(ModModule {
                    range: desugared_mod.range,
                    body,
                    type_ignores: desugared_mod.type_ignores,
                }),
                needs_typhon_runtime,
            }
        }
        other => DesugarOutput {
            module: other.clone(),
            needs_typhon_runtime: false,
        },
    }
}

// ── Result detection ─────────────────────────────────────────────────────────

/// Return `true` if any statement in `stmts` (or its nested bodies) references
/// the identifiers `Ok`, `Err`, or `Result`.
fn stmts_use_result_names(stmts: &[Stmt<TextRange>]) -> bool {
    stmts.iter().any(stmt_uses_result_names)
}

fn stmt_uses_result_names(stmt: &Stmt<TextRange>) -> bool {
    match stmt {
        Stmt::FunctionDef(f) => {
            f.returns
                .as_ref()
                .is_some_and(|r| expr_uses_result_names(r))
                || arguments_use_result_names(&f.args)
                || f.decorator_list.iter().any(expr_uses_result_names)
                || stmts_use_result_names(&f.body)
        }
        Stmt::AsyncFunctionDef(f) => {
            f.returns
                .as_ref()
                .is_some_and(|r| expr_uses_result_names(r))
                || arguments_use_result_names(&f.args)
                || f.decorator_list.iter().any(expr_uses_result_names)
                || stmts_use_result_names(&f.body)
        }
        Stmt::ClassDef(c) => {
            c.decorator_list.iter().any(expr_uses_result_names)
                || c.bases.iter().any(expr_uses_result_names)
                || c.keywords.iter().any(|k| expr_uses_result_names(&k.value))
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
                || stmts_use_result_names(&i.orelse)
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
        Stmt::AsyncFor(f) => {
            expr_uses_result_names(&f.target)
                || expr_uses_result_names(&f.iter)
                || stmts_use_result_names(&f.body)
                || stmts_use_result_names(&f.orelse)
        }
        Stmt::With(w) => {
            w.items.iter().any(with_item_uses_result_names) || stmts_use_result_names(&w.body)
        }
        Stmt::AsyncWith(w) => {
            w.items.iter().any(with_item_uses_result_names) || stmts_use_result_names(&w.body)
        }
        Stmt::Try(t) => {
            stmts_use_result_names(&t.body)
                || t.handlers.iter().any(except_handler_uses_result_names)
                || stmts_use_result_names(&t.orelse)
                || stmts_use_result_names(&t.finalbody)
        }
        Stmt::TryStar(t) => {
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

fn arguments_use_result_names(args: &rustpython_ast::Arguments<TextRange>) -> bool {
    let plain_arg_uses = |arg: &rustpython_ast::Arg<TextRange>| {
        arg.annotation
            .as_ref()
            .is_some_and(|a| expr_uses_result_names(a))
    };
    let with_default_uses = |arg: &rustpython_ast::ArgWithDefault<TextRange>| {
        plain_arg_uses(&arg.def)
            || arg
                .default
                .as_ref()
                .is_some_and(|d| expr_uses_result_names(d))
    };
    args.posonlyargs.iter().any(with_default_uses)
        || args.args.iter().any(with_default_uses)
        || args.kwonlyargs.iter().any(with_default_uses)
        || args.vararg.as_ref().is_some_and(|a| plain_arg_uses(a))
        || args.kwarg.as_ref().is_some_and(|a| plain_arg_uses(a))
}

fn with_item_uses_result_names(item: &rustpython_ast::WithItem<TextRange>) -> bool {
    expr_uses_result_names(&item.context_expr)
        || item
            .optional_vars
            .as_ref()
            .is_some_and(|v| expr_uses_result_names(v))
}

fn except_handler_uses_result_names(handler: &rustpython_ast::ExceptHandler<TextRange>) -> bool {
    let rustpython_ast::ExceptHandler::ExceptHandler(h) = handler;
    h.type_.as_ref().is_some_and(|t| expr_uses_result_names(t)) || stmts_use_result_names(&h.body)
}

fn expr_uses_result_names(expr: &Expr<TextRange>) -> bool {
    match expr {
        Expr::Name(n) => matches!(n.id.as_str(), "Ok" | "Err" | "Result"),
        Expr::Call(c) => {
            expr_uses_result_names(&c.func)
                || c.args.iter().any(expr_uses_result_names)
                || c.keywords.iter().any(|k| expr_uses_result_names(&k.value))
        }
        Expr::Subscript(s) => expr_uses_result_names(&s.value) || expr_uses_result_names(&s.slice),
        Expr::BinOp(b) => expr_uses_result_names(&b.left) || expr_uses_result_names(&b.right),
        Expr::BoolOp(b) => b.values.iter().any(expr_uses_result_names),
        Expr::UnaryOp(u) => expr_uses_result_names(&u.operand),
        Expr::NamedExpr(n) => expr_uses_result_names(&n.target) || expr_uses_result_names(&n.value),
        Expr::Compare(c) => {
            expr_uses_result_names(&c.left) || c.comparators.iter().any(expr_uses_result_names)
        }
        Expr::Lambda(l) => expr_uses_result_names(&l.body),
        Expr::IfExp(i) => {
            expr_uses_result_names(&i.test)
                || expr_uses_result_names(&i.body)
                || expr_uses_result_names(&i.orelse)
        }
        Expr::Tuple(t) => t.elts.iter().any(expr_uses_result_names),
        Expr::List(l) => l.elts.iter().any(expr_uses_result_names),
        Expr::Set(s) => s.elts.iter().any(expr_uses_result_names),
        Expr::Dict(d) => {
            d.keys
                .iter()
                .any(|k| k.as_ref().is_some_and(expr_uses_result_names))
                || d.values.iter().any(expr_uses_result_names)
        }
        Expr::ListComp(c) => {
            expr_uses_result_names(&c.elt)
                || c.generators.iter().any(comprehension_uses_result_names)
        }
        Expr::SetComp(c) => {
            expr_uses_result_names(&c.elt)
                || c.generators.iter().any(comprehension_uses_result_names)
        }
        Expr::GeneratorExp(g) => {
            expr_uses_result_names(&g.elt)
                || g.generators.iter().any(comprehension_uses_result_names)
        }
        Expr::DictComp(d) => {
            expr_uses_result_names(&d.key)
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
        Expr::FormattedValue(f) => expr_uses_result_names(&f.value),
        Expr::JoinedStr(j) => j.values.iter().any(expr_uses_result_names),
        Expr::Attribute(a) => expr_uses_result_names(&a.value),
        // Leaf nodes that cannot contain Result names: constants, etc.
        _ => false,
    }
}

fn comprehension_uses_result_names(gen: &rustpython_ast::Comprehension<TextRange>) -> bool {
    expr_uses_result_names(&gen.target)
        || expr_uses_result_names(&gen.iter)
        || gen.ifs.iter().any(expr_uses_result_names)
}

/// Return `true` if `body` contains any reference to the `typhon_runtime`
/// module — either `import typhon_runtime` or `from typhon_runtime import …`.
/// When true, the build must still write the runtime helper file even if
/// no Ok/Err/Result names appear directly in expressions (the user may be
/// calling `typhon_runtime.Ok(...)` via the bare-import / qualified style).
fn has_any_typhon_runtime_import(body: &[Stmt<TextRange>]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::Import(imp) => imp
            .names
            .iter()
            .any(|a| a.name.as_str() == "typhon_runtime"),
        Stmt::ImportFrom(imp) => imp.module.as_deref() == Some("typhon_runtime"),
        _ => false,
    })
}

/// Return `true` if an existing `from typhon_runtime import …` already brings
/// all three runtime names (`Ok`, `Err`, `Result`) into scope. Used to skip
/// injection only when the user-provided import is complete; partial imports
/// (e.g. just `Ok`) still need injection so the missing names resolve.
fn typhon_runtime_import_covers_all(body: &[Stmt<TextRange>]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::ImportFrom(imp) if imp.module.as_deref() == Some("typhon_runtime") => {
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
fn make_typhon_runtime_import() -> Stmt<TextRange> {
    Stmt::ImportFrom(StmtImportFrom {
        range: TextRange::default(),
        module: Some(Identifier::new("typhon_runtime")),
        names: vec![
            Alias {
                range: TextRange::default(),
                name: Identifier::new("Ok"),
                asname: None,
            },
            Alias {
                range: TextRange::default(),
                name: Identifier::new("Err"),
                asname: None,
            },
            Alias {
                range: TextRange::default(),
                name: Identifier::new("Result"),
                asname: None,
            },
        ],
        level: None,
    })
}

// ── module-level desugaring ──────────────────────────────────────────────────

fn desugar_mod_module(m: &ModModule<TextRange>) -> ModModule<TextRange> {
    let (new_body, transformed_classes) = desugar_stmts(&m.body);

    // Merge `impl` pseudo-classes into their target classes and remove the stubs.
    let (merged_body, _) = merge_impl_blocks(new_body);

    let final_body = if transformed_classes && !has_dataclasses_import(&m.body) {
        let insert_at = import_insert_pos(&merged_body);
        let mut body = merged_body;
        body.insert(insert_at, make_dataclasses_import());
        body
    } else {
        merged_body
    };

    ModModule {
        range: m.range,
        body: final_body,
        type_ignores: m.type_ignores.clone(),
    }
}

/// Return the index at which a new top-level import should be inserted,
/// skipping past an optional module docstring and any `from __future__ import`
/// statements (both must remain at the top of a Python module).
fn import_insert_pos(body: &[Stmt<TextRange>]) -> usize {
    let mut pos = 0;

    // Skip optional module docstring (a bare string-constant expression).
    if let Some(Stmt::Expr(e)) = body.first() {
        if matches!(&*e.value, rustpython_ast::Expr::Constant(c) if matches!(c.value, Constant::Str(_)))
        {
            pos = 1;
        }
    }

    // Skip `from __future__ import ...` statements.
    while pos < body.len() {
        if let Stmt::ImportFrom(imp) = &body[pos] {
            if imp.module.as_deref() == Some("__future__") {
                pos += 1;
                continue;
            }
        }
        break;
    }

    pos
}

// ── recursive statement desugaring ──────────────────────────────────────────

/// Desugar a list of statements, returning the transformed list and whether
/// any class was modified at any nesting depth.
fn desugar_stmts(stmts: &[Stmt<TextRange>]) -> (Vec<Stmt<TextRange>>, bool) {
    let mut any_transformed = false;
    let new_stmts = stmts
        .iter()
        .map(|stmt| {
            let (new_stmt, transformed) = desugar_stmt(stmt);
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
/// transformed at this level or deeper.
fn desugar_stmt(stmt: &Stmt<TextRange>) -> (Stmt<TextRange>, bool) {
    match stmt {
        Stmt::ClassDef(c) => {
            let is_pydantic = class_inherits_basemodel(c);
            // `impl` pseudo-classes (`__typhon_impl_*`) are temporary stubs
            // that will be merged into their target class by `merge_impl_blocks`;
            // they must not receive a dataclass decorator.
            let is_impl_stub = c.name.as_str().starts_with("__typhon_impl_");
            // Skip the dataclass decorator for Pydantic model classes; they
            // already carry the right base class from preprocessing.
            let needs_decorator =
                !is_pydantic && !is_impl_stub && !has_dataclass_decorator(&c.decorator_list);
            // Pydantic `model` classes must have `model_config = ConfigDict(extra="forbid")`
            // as their first body statement unless the user already defined it.
            let needs_model_config =
                is_pydantic && !is_impl_stub && !has_model_config_stmt(&c.body);
            let (new_body, body_transformed) = desugar_stmts(&c.body);
            let mut new_class = c.clone();
            new_class.body = new_body;
            if needs_decorator {
                new_class
                    .decorator_list
                    .insert(0, make_dataclasses_dot_dataclass_call());
            }
            if needs_model_config {
                // Insert after a leading class docstring so that `__doc__` is
                // preserved.  Python requires the docstring to be the first
                // statement in the class body.
                let insert_at = if let Some(Stmt::Expr(e)) = new_class.body.first() {
                    if matches!(&*e.value, Expr::Constant(c) if matches!(c.value, Constant::Str(_)))
                    {
                        1
                    } else {
                        0
                    }
                } else {
                    0
                };
                new_class.body.insert(insert_at, make_model_config_stmt());
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
            let (new_body, transformed) = desugar_stmts(&f.body);
            let mut new_f = f.clone();
            new_f.body = new_body;
            (Stmt::FunctionDef(new_f), transformed)
        }
        Stmt::AsyncFunctionDef(f) => {
            let (new_body, transformed) = desugar_stmts(&f.body);
            let mut new_f = f.clone();
            new_f.body = new_body;
            (Stmt::AsyncFunctionDef(new_f), transformed)
        }
        other => (other.clone(), false),
    }
}

// ── AST helpers ──────────────────────────────────────────────────────────────

/// Build the expression `dataclasses.dataclass(slots=True)`.
///
/// Using the qualified form avoids shadowing: even if the user has a local
/// binding named `dataclass`, `dataclasses.dataclass` still resolves to the
/// standard-library function.
fn make_dataclasses_dot_dataclass_call() -> rustpython_ast::Expr<TextRange> {
    use rustpython_ast::{ExprAttribute, Keyword};

    let dataclasses_name = rustpython_ast::Expr::Name(ExprName {
        range: TextRange::default(),
        id: Identifier::new("dataclasses"),
        ctx: ExprContext::Load,
    });

    let dataclass_attr = rustpython_ast::Expr::Attribute(ExprAttribute {
        range: TextRange::default(),
        value: Box::new(dataclasses_name),
        attr: Identifier::new("dataclass"),
        ctx: ExprContext::Load,
    });

    rustpython_ast::Expr::Call(ExprCall {
        range: TextRange::default(),
        func: Box::new(dataclass_attr),
        args: vec![],
        keywords: vec![Keyword {
            range: TextRange::default(),
            arg: Some(Identifier::new("slots")),
            value: rustpython_ast::Expr::Constant(ExprConstant {
                range: TextRange::default(),
                value: Constant::Bool(true),
                kind: None,
            }),
        }],
    })
}

/// Build the statement `import dataclasses`.
fn make_dataclasses_import() -> Stmt<TextRange> {
    use rustpython_ast::Alias;

    Stmt::Import(StmtImport {
        range: TextRange::default(),
        names: vec![Alias {
            range: TextRange::default(),
            name: Identifier::new("dataclasses"),
            asname: None,
        }],
    })
}

/// Return `true` if any statement in `stmts` (recursively) uses `BaseModel`
/// as a base class — i.e. the module was produced from `model` keywords.
fn stmts_use_basemodel(stmts: &[Stmt<TextRange>]) -> bool {
    stmts.iter().any(|stmt| match stmt {
        Stmt::ClassDef(c) => class_inherits_basemodel(c) || stmts_use_basemodel(&c.body),
        Stmt::FunctionDef(f) => stmts_use_basemodel(&f.body),
        Stmt::AsyncFunctionDef(f) => stmts_use_basemodel(&f.body),
        Stmt::If(s) => stmts_use_basemodel(&s.body) || stmts_use_basemodel(&s.orelse),
        Stmt::For(s) => stmts_use_basemodel(&s.body) || stmts_use_basemodel(&s.orelse),
        Stmt::AsyncFor(s) => stmts_use_basemodel(&s.body) || stmts_use_basemodel(&s.orelse),
        Stmt::While(s) => stmts_use_basemodel(&s.body) || stmts_use_basemodel(&s.orelse),
        Stmt::With(s) => stmts_use_basemodel(&s.body),
        Stmt::AsyncWith(s) => stmts_use_basemodel(&s.body),
        Stmt::Try(s) => {
            stmts_use_basemodel(&s.body)
                || s.handlers.iter().any(|h| match h {
                    rustpython_ast::ExceptHandler::ExceptHandler(eh) => {
                        stmts_use_basemodel(&eh.body)
                    }
                })
                || stmts_use_basemodel(&s.orelse)
                || stmts_use_basemodel(&s.finalbody)
        }
        _ => false,
    })
}

/// Return `true` if `c` inherits directly from `BaseModel`.
fn class_inherits_basemodel(c: &rustpython_ast::StmtClassDef<TextRange>) -> bool {
    c.bases
        .iter()
        .any(|base| matches!(base, rustpython_ast::Expr::Name(n) if n.id.as_str() == "BaseModel"))
}

/// Return `true` if the module already has `from pydantic import BaseModel`
/// where the name is bound as `BaseModel` (not aliased to something else).
fn has_pydantic_import(body: &[Stmt<TextRange>]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::ImportFrom(imp) => {
            imp.module.as_deref() == Some("pydantic")
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
fn make_pydantic_basemodel_import() -> Stmt<TextRange> {
    Stmt::ImportFrom(StmtImportFrom {
        range: TextRange::default(),
        module: Some(Identifier::new("pydantic")),
        names: vec![
            Alias {
                range: TextRange::default(),
                name: Identifier::new("BaseModel"),
                asname: None,
            },
            Alias {
                range: TextRange::default(),
                name: Identifier::new("ConfigDict"),
                asname: None,
            },
        ],
        level: None,
    })
}

/// Build `from pydantic import BaseModel` (without ConfigDict).
///
/// Used when only BaseModel is missing — ConfigDict is already imported.
fn make_pydantic_basemodel_only_import() -> Stmt<TextRange> {
    Stmt::ImportFrom(StmtImportFrom {
        range: TextRange::default(),
        module: Some(Identifier::new("pydantic")),
        names: vec![Alias {
            range: TextRange::default(),
            name: Identifier::new("BaseModel"),
            asname: None,
        }],
        level: None,
    })
}

/// Build `from pydantic import ConfigDict` (without BaseModel).
///
/// Used when a module already imports `BaseModel` explicitly but does not yet
/// import `ConfigDict`, which is needed for the injected `model_config` statement.
fn make_config_dict_only_import() -> Stmt<TextRange> {
    Stmt::ImportFrom(StmtImportFrom {
        range: TextRange::default(),
        module: Some(Identifier::new("pydantic")),
        names: vec![Alias {
            range: TextRange::default(),
            name: Identifier::new("ConfigDict"),
            asname: None,
        }],
        level: None,
    })
}

/// Return `true` if `body` already imports `ConfigDict` from `pydantic`.
fn has_config_dict_import(body: &[Stmt<TextRange>]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::ImportFrom(imp) => {
            imp.module.as_deref() == Some("pydantic")
                && imp
                    .names
                    .iter()
                    .any(|a| matches!(a.name.as_str(), "ConfigDict" | "*"))
        }
        _ => false,
    })
}

/// Return `true` if `body` already contains a `model_config = ...` statement.
fn has_model_config_stmt(body: &[Stmt<TextRange>]) -> bool {
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
fn make_model_config_stmt() -> Stmt<TextRange> {
    use rustpython_ast::Keyword;

    let config_dict_call = Expr::Call(ExprCall {
        range: TextRange::default(),
        func: Box::new(Expr::Name(ExprName {
            range: TextRange::default(),
            id: Identifier::new("ConfigDict"),
            ctx: ExprContext::Load,
        })),
        args: vec![],
        keywords: vec![Keyword {
            range: TextRange::default(),
            arg: Some(Identifier::new("extra")),
            value: Expr::Constant(ExprConstant {
                range: TextRange::default(),
                value: Constant::Str("forbid".to_string()),
                kind: None,
            }),
        }],
    });

    Stmt::Assign(StmtAssign {
        range: TextRange::default(),
        targets: vec![Expr::Name(ExprName {
            range: TextRange::default(),
            id: Identifier::new("model_config"),
            ctx: ExprContext::Store,
        })],
        value: Box::new(config_dict_call),
        type_comment: None,
    })
}

/// Return `true` if the decorator list already contains any recognized form of
/// the dataclass decorator:
/// - `@dataclass`          (bare name, from-import style)
/// - `@dataclass(...)`     (call, from-import style)
/// - `@dataclasses.dataclass`
/// - `@dataclasses.dataclass(...)`
fn has_dataclass_decorator(decorators: &[rustpython_ast::Expr<TextRange>]) -> bool {
    decorators.iter().any(is_dataclass_expr)
}

fn is_dataclass_expr(expr: &rustpython_ast::Expr<TextRange>) -> bool {
    match expr {
        // @dataclass
        rustpython_ast::Expr::Name(n) => n.id.as_str() == "dataclass",
        // @dataclasses.dataclass
        rustpython_ast::Expr::Attribute(a) => {
            a.attr.as_str() == "dataclass"
                && matches!(a.value.as_ref(),
                    rustpython_ast::Expr::Name(n) if n.id.as_str() == "dataclasses"
                )
        }
        // @dataclass(...) or @dataclasses.dataclass(...)
        rustpython_ast::Expr::Call(c) => is_dataclass_expr(c.func.as_ref()),
        _ => false,
    }
}

/// Return `true` if the body already contains `import dataclasses` or
/// `from dataclasses import dataclass` (either form means the import is covered).
fn has_dataclasses_import(body: &[Stmt<TextRange>]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::Import(imp) => imp.names.iter().any(|a| a.name.as_str() == "dataclasses"),
        Stmt::ImportFrom(imp) => {
            imp.module.as_deref() == Some("dataclasses")
                && imp.names.iter().any(|a| a.name.as_str() == "dataclass")
        }
        _ => false,
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
fn merge_impl_blocks(body: Vec<Stmt<TextRange>>) -> (Vec<Stmt<TextRange>>, bool) {
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
    let mut impl_methods_map: HashMap<String, Vec<Stmt<TextRange>>> = HashMap::new();
    for (impl_idx, target_name) in &impl_indices {
        if let Stmt::ClassDef(c) = &body[*impl_idx] {
            let methods: Vec<Stmt<TextRange>> = c.body.iter().map(insert_self_param).collect();
            impl_methods_map
                .entry(target_name.clone())
                .or_default()
                .extend(methods);
        }
    }

    // Phase 3: rebuild the body, merging methods into target classes and
    // dropping the impl pseudo-classes.
    let new_body: Vec<Stmt<TextRange>> = body
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
/// Handles both `Stmt::FunctionDef` and `Stmt::AsyncFunctionDef`; all other
/// statement kinds pass through unchanged.
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
fn insert_self_param(stmt: &Stmt<TextRange>) -> Stmt<TextRange> {
    fn make_receiver_arg(name: &'static str) -> ArgWithDefault<TextRange> {
        ArgWithDefault {
            range: Default::default(),
            def: Arg {
                range: TextRange::default(),
                arg: Identifier::new(name),
                annotation: None,
                type_comment: None,
            },
            default: None,
        }
    }

    fn first_param_name(args: &rustpython_ast::Arguments<TextRange>) -> Option<&str> {
        args.posonlyargs
            .first()
            .or_else(|| args.args.first())
            .map(|a| a.def.arg.as_str())
    }

    fn decorator_name(d: &Expr<TextRange>) -> Option<&str> {
        match d {
            Expr::Name(n) => Some(n.id.as_str()),
            Expr::Call(c) => decorator_name(&c.func),
            _ => None,
        }
    }

    fn receiver_for(decorators: &[Expr<TextRange>]) -> Option<&'static str> {
        for d in decorators {
            match decorator_name(d) {
                Some("staticmethod") => return None, // no receiver
                Some("classmethod") => return Some("cls"),
                _ => {}
            }
        }
        Some("self")
    }

    fn inject(args: &mut rustpython_ast::Arguments<TextRange>, receiver: &'static str) {
        if first_param_name(args) == Some(receiver) {
            return; // already present — do not duplicate
        }
        let param = make_receiver_arg(receiver);
        if args.posonlyargs.is_empty() {
            args.args.insert(0, param);
        } else {
            args.posonlyargs.insert(0, param);
        }
    }

    match stmt {
        Stmt::FunctionDef(f) => {
            let mut new_f = f.clone();
            if let Some(receiver) = receiver_for(&new_f.decorator_list) {
                inject(&mut new_f.args, receiver);
            }
            Stmt::FunctionDef(new_f)
        }
        Stmt::AsyncFunctionDef(f) => {
            let mut new_f = f.clone();
            if let Some(receiver) = receiver_for(&new_f.decorator_list) {
                inject(&mut new_f.args, receiver);
            }
            Stmt::AsyncFunctionDef(new_f)
        }
        other => other.clone(),
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rustpython_parser::{parse, Mode};
    use tyc_emit::emit;

    fn parse_and_desugar(src: &str) -> String {
        let m = parse(src, Mode::Module, "<test>").expect("parse failed");
        let output = desugar_module(&m);
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
        let m = parse(src, Mode::Module, "<test>").expect("parse failed");
        let output = desugar_module(&m);
        assert!(
            output.needs_typhon_runtime,
            "flag should be true when Ok is used"
        );
    }

    #[test]
    fn needs_typhon_runtime_flag_clear_when_result_not_used() {
        let src = "x: int = 1\n";
        let m = parse(src, Mode::Module, "<test>").expect("parse failed");
        let output = desugar_module(&m);
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
        let m = parse(src, Mode::Module, "<test>").expect("parse failed");
        let output = desugar_module(&m);
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
}
