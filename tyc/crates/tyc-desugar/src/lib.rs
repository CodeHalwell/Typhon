//! Typhon AST → Python AST lowering (Phase 2+).
//!
//! Implements three transformations over the Python-compatible AST produced
//! after preprocessing:
//!
//! 1. **Class desugaring** — every plain `class` definition without a
//!    `@dataclass` decorator gets `@dataclasses.dataclass(slots=True)` prepended
//!    and `import dataclasses` is injected when needed.
//!
//! 2. **Pydantic model desugaring** — classes that the preprocessor tagged with
//!    `__TyphonModel__` as a base (from `model X:` syntax) are rewritten to
//!    `class X(BaseModel):` and `from pydantic import BaseModel` is injected.
//!
//! 3. **Result import injection** — if the module references `Ok`, `Err`, or
//!    `Result` anywhere, `from typhon_runtime import Ok, Err, Result` is injected
//!    after any leading docstring and future-imports.
//!
//! 4. **`?` try-operator expansion** — `call().__typhon_try__()` expressions
//!    (produced by the preprocessor from `call()?` syntax) are expanded into the
//!    early-return pattern:
//!    ```python
//!    __typhon_tmp_N = call()
//!    if isinstance(__typhon_tmp_N, Err):
//!        return __typhon_tmp_N
//!    x = __typhon_tmp_N.value          # only for assignments
//!    ```
//!    The transformation also sets `needs_typhon_runtime` so the build emits
//!    `typhon_runtime.py` alongside the output.

use rustpython_ast::{
    text_size::TextRange, Alias, Constant, Expr, ExprAttribute, ExprCall, ExprConstant,
    ExprContext, ExprName, Identifier, Mod, ModModule, Stmt, StmtAssign, StmtIf, StmtImport,
    StmtImportFrom, StmtReturn,
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
    /// Whether the emitted module imports from `pydantic`. When true, the
    /// consuming project must have `pydantic` installed (it is not bundled).
    pub needs_pydantic: bool,
}

/// Desugar a Typhon module AST into a plain Python AST.
pub fn desugar_module(module: &Mod<TextRange>) -> DesugarOutput {
    match module {
        Mod::Module(m) => {
            let (desugared_mod, desugar_stats) = desugar_mod_module(m);

            let has_result_usage = stmts_use_result_names(&m.body);
            let has_any_runtime_import = has_any_typhon_runtime_import(&m.body);
            let import_covers_all = typhon_runtime_import_covers_all(&m.body);

            // The build must write `typhon_runtime.py` whenever the emitted
            // module references Ok/Err/Result (directly or via existing import)
            // or whenever `?` try-operators were expanded (which generates
            // isinstance(_, Err) checks).
            let needs_typhon_runtime =
                has_result_usage || has_any_runtime_import || desugar_stats.any_try_operator;

            // Inject the full three-name import only when names are used and
            // no complete import already covers them.
            let inject_import =
                (has_result_usage || desugar_stats.any_try_operator) && !import_covers_all;

            let final_body = if inject_import {
                let insert_at = import_insert_pos(&desugared_mod.body);
                let mut body = desugared_mod.body;
                body.insert(insert_at, make_typhon_runtime_import());
                body
            } else {
                desugared_mod.body
            };

            DesugarOutput {
                module: Mod::Module(ModModule {
                    range: desugared_mod.range,
                    body: final_body,
                    type_ignores: desugared_mod.type_ignores,
                }),
                needs_typhon_runtime,
                needs_pydantic: desugar_stats.any_pydantic_model,
            }
        }
        other => DesugarOutput {
            module: other.clone(),
            needs_typhon_runtime: false,
            needs_pydantic: false,
        },
    }
}

// ── internal statistics ───────────────────────────────────────────────────────

/// Accumulated flags from a single desugaring pass over a list of statements.
#[derive(Default)]
struct DesugarStats {
    /// At least one plain class was given `@dataclasses.dataclass`.
    any_class_dataclassed: bool,
    /// At least one `model X:` class was rewritten to `class X(BaseModel):`.
    any_pydantic_model: bool,
    /// At least one `.__typhon_try__()` call was expanded.
    any_try_operator: bool,
}

impl DesugarStats {
    fn merge(&mut self, other: DesugarStats) {
        self.any_class_dataclassed |= other.any_class_dataclassed;
        self.any_pydantic_model |= other.any_pydantic_model;
        self.any_try_operator |= other.any_try_operator;
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
            f.returns.as_ref().map_or(false, |r| expr_uses_result_names(r))
                || arguments_use_result_names(&f.args)
                || f.decorator_list.iter().any(expr_uses_result_names)
                || stmts_use_result_names(&f.body)
        }
        Stmt::AsyncFunctionDef(f) => {
            f.returns.as_ref().map_or(false, |r| expr_uses_result_names(r))
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
                || a.value.as_ref().map_or(false, |v| expr_uses_result_names(v))
                || expr_uses_result_names(&a.target)
        }
        Stmt::Assign(a) => {
            expr_uses_result_names(&a.value)
                || a.targets.iter().any(expr_uses_result_names)
        }
        Stmt::AugAssign(a) => {
            expr_uses_result_names(&a.target) || expr_uses_result_names(&a.value)
        }
        Stmt::Return(r) => r.value.as_ref().map_or(false, |v| expr_uses_result_names(v)),
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
            w.items.iter().any(with_item_uses_result_names)
                || stmts_use_result_names(&w.body)
        }
        Stmt::AsyncWith(w) => {
            w.items.iter().any(with_item_uses_result_names)
                || stmts_use_result_names(&w.body)
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
                        .map_or(false, |g| expr_uses_result_names(g))
                        || stmts_use_result_names(&case.body)
                })
        }
        Stmt::Raise(r) => {
            r.exc.as_ref().map_or(false, |e| expr_uses_result_names(e))
                || r.cause.as_ref().map_or(false, |c| expr_uses_result_names(c))
        }
        Stmt::Assert(a) => {
            expr_uses_result_names(&a.test)
                || a.msg.as_ref().map_or(false, |m| expr_uses_result_names(m))
        }
        Stmt::Delete(d) => d.targets.iter().any(expr_uses_result_names),
        Stmt::TypeAlias(t) => {
            expr_uses_result_names(&t.name) || expr_uses_result_names(&t.value)
        }
        _ => false,
    }
}

fn arguments_use_result_names(args: &rustpython_ast::Arguments<TextRange>) -> bool {
    let plain_arg_uses = |arg: &rustpython_ast::Arg<TextRange>| {
        arg.annotation
            .as_ref()
            .map_or(false, |a| expr_uses_result_names(a))
    };
    let with_default_uses = |arg: &rustpython_ast::ArgWithDefault<TextRange>| {
        plain_arg_uses(&arg.def)
            || arg.default.as_ref().map_or(false, |d| expr_uses_result_names(d))
    };
    args.posonlyargs.iter().any(with_default_uses)
        || args.args.iter().any(with_default_uses)
        || args.kwonlyargs.iter().any(with_default_uses)
        || args.vararg.as_ref().map_or(false, |a| plain_arg_uses(a))
        || args.kwarg.as_ref().map_or(false, |a| plain_arg_uses(a))
}

fn with_item_uses_result_names(item: &rustpython_ast::WithItem<TextRange>) -> bool {
    expr_uses_result_names(&item.context_expr)
        || item
            .optional_vars
            .as_ref()
            .map_or(false, |v| expr_uses_result_names(v))
}

fn except_handler_uses_result_names(
    handler: &rustpython_ast::ExceptHandler<TextRange>,
) -> bool {
    let rustpython_ast::ExceptHandler::ExceptHandler(h) = handler;
    h.type_
        .as_ref()
        .map_or(false, |t| expr_uses_result_names(t))
        || stmts_use_result_names(&h.body)
}

fn expr_uses_result_names(expr: &Expr<TextRange>) -> bool {
    match expr {
        Expr::Name(n) => matches!(n.id.as_str(), "Ok" | "Err" | "Result"),
        Expr::Call(c) => {
            expr_uses_result_names(&c.func)
                || c.args.iter().any(expr_uses_result_names)
                || c.keywords.iter().any(|k| expr_uses_result_names(&k.value))
        }
        Expr::Subscript(s) => {
            expr_uses_result_names(&s.value) || expr_uses_result_names(&s.slice)
        }
        Expr::BinOp(b) => expr_uses_result_names(&b.left) || expr_uses_result_names(&b.right),
        Expr::BoolOp(b) => b.values.iter().any(expr_uses_result_names),
        Expr::UnaryOp(u) => expr_uses_result_names(&u.operand),
        Expr::NamedExpr(n) => {
            expr_uses_result_names(&n.target) || expr_uses_result_names(&n.value)
        }
        Expr::Compare(c) => {
            expr_uses_result_names(&c.left)
                || c.comparators.iter().any(expr_uses_result_names)
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
                .any(|k| k.as_ref().map_or(false, expr_uses_result_names))
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
        Expr::Yield(y) => y.value.as_ref().map_or(false, |v| expr_uses_result_names(v)),
        Expr::YieldFrom(y) => expr_uses_result_names(&y.value),
        Expr::Starred(s) => expr_uses_result_names(&s.value),
        Expr::Slice(s) => {
            s.lower.as_ref().map_or(false, |e| expr_uses_result_names(e))
                || s.upper.as_ref().map_or(false, |e| expr_uses_result_names(e))
                || s.step.as_ref().map_or(false, |e| expr_uses_result_names(e))
        }
        Expr::FormattedValue(f) => expr_uses_result_names(&f.value),
        Expr::JoinedStr(j) => j.values.iter().any(expr_uses_result_names),
        Expr::Attribute(a) => expr_uses_result_names(&a.value),
        _ => false,
    }
}

fn comprehension_uses_result_names(
    gen: &rustpython_ast::Comprehension<TextRange>,
) -> bool {
    expr_uses_result_names(&gen.target)
        || expr_uses_result_names(&gen.iter)
        || gen.ifs.iter().any(expr_uses_result_names)
}

/// Return `true` if `body` contains any reference to the `typhon_runtime`
/// module — either `import typhon_runtime` or `from typhon_runtime import …`.
fn has_any_typhon_runtime_import(body: &[Stmt<TextRange>]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::Import(imp) => imp.names.iter().any(|a| a.name.as_str() == "typhon_runtime"),
        Stmt::ImportFrom(imp) => imp.module.as_deref() == Some("typhon_runtime"),
        _ => false,
    })
}

/// Return `true` if an existing `from typhon_runtime import …` already brings
/// all three runtime names (`Ok`, `Err`, `Result`) into scope.
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

fn desugar_mod_module(m: &ModModule<TextRange>) -> (ModModule<TextRange>, DesugarStats) {
    let mut counter = 0usize;
    let (new_body, stats) = desugar_stmts(&m.body, &mut counter);

    // Inject `import dataclasses` when plain classes were desugared.
    let body_after_dc = if stats.any_class_dataclassed && !has_dataclasses_import(&m.body) {
        let insert_at = import_insert_pos(&new_body);
        let mut body = new_body;
        body.insert(insert_at, make_dataclasses_import());
        body
    } else {
        new_body
    };

    // Inject `from pydantic import BaseModel` when `model` classes were desugared.
    let final_body = if stats.any_pydantic_model && !has_pydantic_import(&m.body) {
        let insert_at = import_insert_pos(&body_after_dc);
        let mut body = body_after_dc;
        body.insert(insert_at, make_pydantic_import());
        body
    } else {
        body_after_dc
    };

    (
        ModModule {
            range: m.range,
            body: final_body,
            type_ignores: m.type_ignores.clone(),
        },
        stats,
    )
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

/// Desugar a list of statements, flat-mapping each statement to zero or more
/// statements (needed for the `?` try-operator which expands 1 → 3 stmts).
fn desugar_stmts(
    stmts: &[Stmt<TextRange>],
    counter: &mut usize,
) -> (Vec<Stmt<TextRange>>, DesugarStats) {
    let mut stats = DesugarStats::default();
    let new_stmts = stmts
        .iter()
        .flat_map(|stmt| {
            let (expanded, stmt_stats) = expand_stmt(stmt, counter);
            stats.merge(stmt_stats);
            expanded
        })
        .collect();
    (new_stmts, stats)
}

/// Expand one statement to a (possibly longer) list of statements.
///
/// The `?` try-operator can expand a single assignment into three statements.
/// All other statements map 1-to-1 via `desugar_single_stmt`.
fn expand_stmt(
    stmt: &Stmt<TextRange>,
    counter: &mut usize,
) -> (Vec<Stmt<TextRange>>, DesugarStats) {
    // Check for `?` try-operator patterns first.
    if let Some((expanded, try_stats)) = try_expand_try_operator(stmt, counter) {
        return (expanded, try_stats);
    }
    // Regular desugaring.
    let (new_stmt, stmt_stats) = desugar_single_stmt(stmt, counter);
    (vec![new_stmt], stmt_stats)
}

/// Desugar a single statement that does not require expansion.
/// Recurses into nested statement lists for functions, classes, and control
/// flow so that classes and `?` operators anywhere in the tree are handled.
fn desugar_single_stmt(
    stmt: &Stmt<TextRange>,
    counter: &mut usize,
) -> (Stmt<TextRange>, DesugarStats) {
    match stmt {
        Stmt::ClassDef(c) => {
            if is_pydantic_model_class(c) {
                // `model X:` → `class X(BaseModel):`
                let (new_body, mut body_stats) = desugar_stmts(&c.body, counter);
                let mut new_class = c.clone();
                new_class.body = new_body;
                // Remove __TyphonModel__ sentinel, prepend BaseModel.
                new_class.bases = c
                    .bases
                    .iter()
                    .filter(|b| !is_typhon_model_sentinel(b))
                    .cloned()
                    .collect();
                new_class.bases.insert(0, make_base_model_name());
                body_stats.any_pydantic_model = true;
                (Stmt::ClassDef(new_class), body_stats)
            } else {
                // Plain class → add @dataclasses.dataclass(slots=True).
                let needs_decorator = !has_dataclass_decorator(&c.decorator_list);
                let (new_body, mut body_stats) = desugar_stmts(&c.body, counter);
                let mut new_class = c.clone();
                new_class.body = new_body;
                if needs_decorator {
                    new_class
                        .decorator_list
                        .insert(0, make_dataclasses_dot_dataclass_call());
                    body_stats.any_class_dataclassed = true;
                }
                (Stmt::ClassDef(new_class), body_stats)
            }
        }
        Stmt::FunctionDef(f) => {
            let (new_body, stats) = desugar_stmts(&f.body, counter);
            let mut new_f = f.clone();
            new_f.body = new_body;
            (Stmt::FunctionDef(new_f), stats)
        }
        Stmt::AsyncFunctionDef(f) => {
            let (new_body, stats) = desugar_stmts(&f.body, counter);
            let mut new_f = f.clone();
            new_f.body = new_body;
            (Stmt::AsyncFunctionDef(new_f), stats)
        }
        Stmt::If(i) => {
            let (new_body, body_stats) = desugar_stmts(&i.body, counter);
            let (new_orelse, orelse_stats) = desugar_stmts(&i.orelse, counter);
            let mut new_if = i.clone();
            new_if.body = new_body;
            new_if.orelse = new_orelse;
            let mut stats = body_stats;
            stats.merge(orelse_stats);
            (Stmt::If(new_if), stats)
        }
        Stmt::While(w) => {
            let (new_body, body_stats) = desugar_stmts(&w.body, counter);
            let (new_orelse, orelse_stats) = desugar_stmts(&w.orelse, counter);
            let mut new_while = w.clone();
            new_while.body = new_body;
            new_while.orelse = new_orelse;
            let mut stats = body_stats;
            stats.merge(orelse_stats);
            (Stmt::While(new_while), stats)
        }
        Stmt::For(f) => {
            let (new_body, body_stats) = desugar_stmts(&f.body, counter);
            let (new_orelse, orelse_stats) = desugar_stmts(&f.orelse, counter);
            let mut new_for = f.clone();
            new_for.body = new_body;
            new_for.orelse = new_orelse;
            let mut stats = body_stats;
            stats.merge(orelse_stats);
            (Stmt::For(new_for), stats)
        }
        other => (other.clone(), DesugarStats::default()),
    }
}

// ── `?` try-operator expansion ────────────────────────────────────────────────

/// If `stmt` contains a `.__typhon_try__()` call in a supported position,
/// expand it into the early-return pattern and return the replacement
/// statements. Returns `None` for statements without the try-operator.
///
/// Supported positions:
/// - `x = expr.__typhon_try__()` (simple assignment, single target)
/// - `return expr.__typhon_try__()`
/// - `expr.__typhon_try__()` (bare expression statement — discards Ok value)
fn try_expand_try_operator(
    stmt: &Stmt<TextRange>,
    counter: &mut usize,
) -> Option<(Vec<Stmt<TextRange>>, DesugarStats)> {
    let try_stats = || {
        let mut s = DesugarStats::default();
        s.any_try_operator = true;
        s
    };

    match stmt {
        Stmt::Assign(a) if a.targets.len() == 1 => {
            extract_try_inner(&a.value).map(|inner| {
                let tmp = format!("__typhon_tmp_{}", counter);
                *counter += 1;
                let stmts = vec![
                    make_tmp_assign(&tmp, inner),
                    make_err_check_return(&tmp),
                    make_value_assign(&a.targets[0], &tmp),
                ];
                (stmts, try_stats())
            })
        }
        Stmt::Return(r) => r.value.as_ref().and_then(|v| {
            extract_try_inner(v).map(|inner| {
                let tmp = format!("__typhon_tmp_{}", counter);
                *counter += 1;
                let stmts = vec![
                    make_tmp_assign(&tmp, inner),
                    make_err_check_return(&tmp),
                    make_return_ok_value(&tmp),
                ];
                (stmts, try_stats())
            })
        }),
        Stmt::Expr(e) => {
            extract_try_inner(&e.value).map(|inner| {
                let tmp = format!("__typhon_tmp_{}", counter);
                *counter += 1;
                // Bare try — ok value is discarded; only the Err check matters.
                let stmts = vec![
                    make_tmp_assign(&tmp, inner),
                    make_err_check_return(&tmp),
                ];
                (stmts, try_stats())
            })
        }
        _ => None,
    }
}

/// If `expr` is `something.__typhon_try__()` with no arguments, return
/// `something`. Otherwise return `None`.
fn extract_try_inner(expr: &Expr<TextRange>) -> Option<&Expr<TextRange>> {
    if let Expr::Call(c) = expr {
        if c.args.is_empty() && c.keywords.is_empty() {
            if let Expr::Attribute(a) = c.func.as_ref() {
                if a.attr.as_str() == "__typhon_try__" {
                    return Some(&a.value);
                }
            }
        }
    }
    None
}

// ── AST construction helpers ──────────────────────────────────────────────────

/// Build a `Name` expression with `Load` context.
fn make_name_load(name: &str) -> Expr<TextRange> {
    Expr::Name(ExprName {
        range: TextRange::default(),
        id: Identifier::new(name),
        ctx: ExprContext::Load,
    })
}

/// Build `tmp = value` (simple assignment).
fn make_tmp_assign(tmp: &str, value: &Expr<TextRange>) -> Stmt<TextRange> {
    Stmt::Assign(StmtAssign {
        range: TextRange::default(),
        targets: vec![Expr::Name(ExprName {
            range: TextRange::default(),
            id: Identifier::new(tmp),
            ctx: ExprContext::Store,
        })],
        value: Box::new(value.clone()),
        type_comment: None,
    })
}

/// Build `if isinstance(tmp, Err): return tmp`.
fn make_err_check_return(tmp: &str) -> Stmt<TextRange> {
    let isinstance_call = Expr::Call(ExprCall {
        range: TextRange::default(),
        func: Box::new(make_name_load("isinstance")),
        args: vec![make_name_load(tmp), make_name_load("Err")],
        keywords: vec![],
    });
    Stmt::If(StmtIf {
        range: TextRange::default(),
        test: Box::new(isinstance_call),
        body: vec![Stmt::Return(StmtReturn {
            range: TextRange::default(),
            value: Some(Box::new(make_name_load(tmp))),
        })],
        orelse: vec![],
    })
}

/// Build `target = tmp.value` (unwrap the Ok payload into the original LHS).
fn make_value_assign(target: &Expr<TextRange>, tmp: &str) -> Stmt<TextRange> {
    let value_attr = Expr::Attribute(ExprAttribute {
        range: TextRange::default(),
        value: Box::new(make_name_load(tmp)),
        attr: Identifier::new("value"),
        ctx: ExprContext::Load,
    });
    Stmt::Assign(StmtAssign {
        range: TextRange::default(),
        targets: vec![target.clone()],
        value: Box::new(value_attr),
        type_comment: None,
    })
}

/// Build `return tmp.value` (for `return expr?` context).
fn make_return_ok_value(tmp: &str) -> Stmt<TextRange> {
    let value_attr = Expr::Attribute(ExprAttribute {
        range: TextRange::default(),
        value: Box::new(make_name_load(tmp)),
        attr: Identifier::new("value"),
        ctx: ExprContext::Load,
    });
    Stmt::Return(StmtReturn {
        range: TextRange::default(),
        value: Some(Box::new(value_attr)),
    })
}

// ── pydantic model helpers ────────────────────────────────────────────────────

/// Return `true` if `c` has `__TyphonModel__` as one of its base classes.
fn is_pydantic_model_class(c: &rustpython_ast::StmtClassDef<TextRange>) -> bool {
    c.bases.iter().any(is_typhon_model_sentinel)
}

/// Return `true` if `expr` is the `__TyphonModel__` sentinel name.
fn is_typhon_model_sentinel(expr: &Expr<TextRange>) -> bool {
    matches!(expr, Expr::Name(n) if n.id.as_str() == "__TyphonModel__")
}

/// Build the `BaseModel` name expression (for use as a class base).
fn make_base_model_name() -> Expr<TextRange> {
    Expr::Name(ExprName {
        range: TextRange::default(),
        id: Identifier::new("BaseModel"),
        ctx: ExprContext::Load,
    })
}

/// Build `from pydantic import BaseModel`.
fn make_pydantic_import() -> Stmt<TextRange> {
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

/// Return `true` if the body already contains `from pydantic import BaseModel`.
fn has_pydantic_import(body: &[Stmt<TextRange>]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::ImportFrom(imp) => {
            imp.module.as_deref() == Some("pydantic")
                && imp.names.iter().any(|a| a.name.as_str() == "BaseModel")
        }
        _ => false,
    })
}

// ── dataclass helpers ─────────────────────────────────────────────────────────

/// Build the expression `dataclasses.dataclass(slots=True)`.
fn make_dataclasses_dot_dataclass_call() -> Expr<TextRange> {
    use rustpython_ast::Keyword;

    let dataclasses_name = Expr::Name(ExprName {
        range: TextRange::default(),
        id: Identifier::new("dataclasses"),
        ctx: ExprContext::Load,
    });

    let dataclass_attr = Expr::Attribute(ExprAttribute {
        range: TextRange::default(),
        value: Box::new(dataclasses_name),
        attr: Identifier::new("dataclass"),
        ctx: ExprContext::Load,
    });

    Expr::Call(ExprCall {
        range: TextRange::default(),
        func: Box::new(dataclass_attr),
        args: vec![],
        keywords: vec![Keyword {
            range: TextRange::default(),
            arg: Some(Identifier::new("slots")),
            value: Expr::Constant(ExprConstant {
                range: TextRange::default(),
                value: Constant::Bool(true),
                kind: None,
            }),
        }],
    })
}

/// Build the statement `import dataclasses`.
fn make_dataclasses_import() -> Stmt<TextRange> {
    Stmt::Import(StmtImport {
        range: TextRange::default(),
        names: vec![Alias {
            range: TextRange::default(),
            name: Identifier::new("dataclasses"),
            asname: None,
        }],
    })
}

/// Return `true` if the decorator list already contains any recognized form of
/// the dataclass decorator.
fn has_dataclass_decorator(decorators: &[Expr<TextRange>]) -> bool {
    decorators.iter().any(|d| is_dataclass_expr(d))
}

fn is_dataclass_expr(expr: &Expr<TextRange>) -> bool {
    match expr {
        Expr::Name(n) => n.id.as_str() == "dataclass",
        Expr::Attribute(a) => {
            a.attr.as_str() == "dataclass"
                && matches!(a.value.as_ref(),
                    Expr::Name(n) if n.id.as_str() == "dataclasses"
                )
        }
        Expr::Call(c) => is_dataclass_expr(c.func.as_ref()),
        _ => false,
    }
}

/// Return `true` if the body already contains `import dataclasses` or
/// `from dataclasses import dataclass`.
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
        assert!(out.contains("@dataclasses.dataclass(slots=True)"), "output:\n{out}");
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
        assert_eq!(out.matches("import dataclasses").count(), 1, "output:\n{out}");
    }

    #[test]
    fn class_with_qualified_dataclass_call_not_duplicated() {
        let src = "import dataclasses\n\n@dataclasses.dataclass(slots=True)\nclass Point:\n    x: int\n";
        let out = parse_and_desugar(src);
        assert_eq!(out.matches("@dataclasses.dataclass").count(), 1, "output:\n{out}");
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
        assert_eq!(out.matches("import dataclasses").count(), 1, "output:\n{out}");
        assert_eq!(out.matches("@dataclasses.dataclass(slots=True)").count(), 2, "output:\n{out}");
    }

    #[test]
    fn class_inside_function_is_desugared() {
        let src = "def make_point():\n    class Point:\n        x: int\n    return Point\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("@dataclasses.dataclass(slots=True)"), "output:\n{out}");
        assert!(out.contains("import dataclasses"), "output:\n{out}");
    }

    #[test]
    fn nested_class_inside_class_is_desugared() {
        let src = "class Outer:\n    x: int\n    class Inner:\n        y: int\n";
        let out = parse_and_desugar(src);
        assert_eq!(out.matches("@dataclasses.dataclass(slots=True)").count(), 2, "output:\n{out}");
    }

    #[test]
    fn import_inserted_after_future_imports() {
        let src = "from __future__ import annotations\n\nclass Point:\n    x: int\n";
        let out = parse_and_desugar(src);
        let future_pos = out.find("from __future__").expect("future import missing");
        let import_pos = out.find("import dataclasses").expect("dataclasses import missing");
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
        let import_pos = out.find("import dataclasses").expect("dataclasses import missing");
        assert!(doc_pos < future_pos, "output:\n{out}");
        assert!(future_pos < import_pos, "output:\n{out}");
    }

    // ── pydantic model tests ─────────────────────────────────────────────────

    #[test]
    fn pydantic_model_sentinel_becomes_base_model() {
        // The preprocessor converts `model User:` → `class User(__TyphonModel__):`
        // Desugar must replace the sentinel with BaseModel.
        let src = "class User(__TyphonModel__):\n    id: int\n    name: str\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("class User(BaseModel):"), "output:\n{out}");
        assert!(out.contains("from pydantic import BaseModel"), "output:\n{out}");
    }

    #[test]
    fn pydantic_model_does_not_get_dataclass_decorator() {
        let src = "class ApiUser(__TyphonModel__):\n    id: int\n";
        let out = parse_and_desugar(src);
        assert!(!out.contains("dataclass"), "output:\n{out}");
        assert!(!out.contains("__TyphonModel__"), "sentinel leaked; output:\n{out}");
    }

    #[test]
    fn pydantic_import_not_duplicated() {
        let src =
            "from pydantic import BaseModel\n\nclass User(__TyphonModel__):\n    id: int\n";
        let out = parse_and_desugar(src);
        assert_eq!(
            out.matches("from pydantic import BaseModel").count(),
            1,
            "output:\n{out}"
        );
    }

    #[test]
    fn needs_pydantic_flag_set() {
        let src = "class User(__TyphonModel__):\n    id: int\n";
        let m = parse(src, Mode::Module, "<test>").expect("parse failed");
        let output = desugar_module(&m);
        assert!(output.needs_pydantic, "needs_pydantic should be true");
    }

    #[test]
    fn needs_pydantic_flag_clear_for_plain_class() {
        let src = "class Point:\n    x: int\n";
        let m = parse(src, Mode::Module, "<test>").expect("parse failed");
        let output = desugar_module(&m);
        assert!(!output.needs_pydantic);
    }

    #[test]
    fn model_with_extra_base_preserves_it() {
        // `model X(Mixin, __TyphonModel__):` → `class X(BaseModel, Mixin):`
        // (Mixin is preserved; __TyphonModel__ is removed; BaseModel prepended)
        let src = "class Widget(Mixin, __TyphonModel__):\n    x: int\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("class Widget(BaseModel, Mixin):"), "output:\n{out}");
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
        let src =
            "from typhon_runtime import Ok, Err, Result\n\ndef f() -> None:\n    x = Ok(1)\n";
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
        assert!(output.needs_typhon_runtime, "flag should be true when Ok is used");
    }

    #[test]
    fn needs_typhon_runtime_flag_clear_when_result_not_used() {
        let src = "x: int = 1\n";
        let m = parse(src, Mode::Module, "<test>").expect("parse failed");
        let output = desugar_module(&m);
        assert!(!output.needs_typhon_runtime, "flag should be false when Result not used");
    }

    #[test]
    fn runtime_import_inserted_after_docstring() {
        let src = "\"\"\"Module doc.\"\"\"\ndef f() -> None:\n    x = Ok(1)\n";
        let out = parse_and_desugar(src);
        let doc_pos = out.find("Module doc").expect("docstring missing");
        let import_pos = out.find("from typhon_runtime").expect("runtime import missing");
        assert!(doc_pos < import_pos, "runtime import must follow docstring\noutput:\n{out}");
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
        let src =
            "match x:\n    case 1:\n        r = Ok(1)\n    case _:\n        r = Err('no')\n";
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
        let src =
            "from typhon_runtime import Ok, Err, Result\n\ndef f() -> None:\n    x = Ok(1)\n";
        let out = parse_and_desugar(src);
        assert_eq!(
            out.matches("from typhon_runtime import").count(),
            1,
            "should not duplicate the existing complete import; output:\n{out}"
        );
    }

    #[test]
    fn bare_import_typhon_runtime_sets_needs_flag() {
        let src = "import typhon_runtime\nx = typhon_runtime.Ok(1)\n";
        let m = parse(src, Mode::Module, "<test>").expect("parse failed");
        let output = desugar_module(&m);
        assert!(
            output.needs_typhon_runtime,
            "bare `import typhon_runtime` must set needs_typhon_runtime"
        );
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

    // ── `?` try-operator tests ────────────────────────────────────────────────

    #[test]
    fn try_operator_in_assign_expands() {
        // `x = find(1).__typhon_try__()` should expand to the three-statement pattern.
        let src = "def f() -> None:\n    x = find(1).__typhon_try__()\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("isinstance"), "output:\n{out}");
        assert!(out.contains("Err"), "output:\n{out}");
        assert!(out.contains(".value"), "output:\n{out}");
        assert!(!out.contains("__typhon_try__"), "sentinel leaked; output:\n{out}");
    }

    #[test]
    fn try_operator_sets_needs_typhon_runtime() {
        let src = "def f() -> None:\n    x = find(1).__typhon_try__()\n";
        let m = parse(src, Mode::Module, "<test>").expect("parse failed");
        let output = desugar_module(&m);
        assert!(output.needs_typhon_runtime, "output needs typhon_runtime for Err check");
    }

    #[test]
    fn try_operator_injects_typhon_runtime_import() {
        let src = "def f() -> None:\n    x = find(1).__typhon_try__()\n";
        let out = parse_and_desugar(src);
        assert!(
            out.contains("from typhon_runtime import"),
            "expected runtime import for Err; output:\n{out}"
        );
    }

    #[test]
    fn try_operator_in_return_expands() {
        let src = "def f() -> None:\n    return find(1).__typhon_try__()\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("isinstance"), "output:\n{out}");
        assert!(out.contains(".value"), "output:\n{out}");
    }

    #[test]
    fn try_operator_as_bare_expr_expands() {
        // Bare `expr?` — ok value discarded, only early-return on Err.
        let src = "def f() -> None:\n    find(1).__typhon_try__()\n";
        let out = parse_and_desugar(src);
        assert!(out.contains("isinstance"), "output:\n{out}");
        assert!(!out.contains("__typhon_try__"), "sentinel leaked; output:\n{out}");
    }
}
