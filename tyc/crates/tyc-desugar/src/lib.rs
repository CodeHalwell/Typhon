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

use rustpython_ast::{
    text_size::TextRange, Alias, Constant, Expr, ExprCall, ExprConstant, ExprContext, ExprName,
    Identifier, Mod, ModModule, Stmt, StmtImport, StmtImportFrom,
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
/// Performs two transformations:
///
/// 1. **Class desugaring** — every `class` definition that does not already
///    carry a `@dataclass` / `@dataclasses.dataclass` decorator gets
///    `@dataclasses.dataclass(slots=True)` prepended, and `import dataclasses`
///    is injected when needed. The transformation is recursive.
///
/// 2. **Result import injection** — if the module references `Ok`, `Err`, or
///    `Result` anywhere, `from typhon_runtime import Ok, Err, Result` is
///    injected after any leading docstring and future-imports so the generated
///    Python can use those names.
pub fn desugar_module(module: &Mod<TextRange>) -> DesugarOutput {
    match module {
        Mod::Module(m) => {
            let desugared_mod = desugar_mod_module(m);

            let has_result_usage = stmts_use_result_names(&m.body);
            let has_existing_runtime_import = has_typhon_runtime_import(&m.body);
            let needs_typhon_runtime = has_result_usage || has_existing_runtime_import;
            let inject_import = has_result_usage && !has_existing_runtime_import;

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
            f.returns.as_ref().map_or(false, |r| expr_uses_result_names(r))
                || stmts_use_result_names(&f.body)
        }
        Stmt::AsyncFunctionDef(f) => {
            f.returns.as_ref().map_or(false, |r| expr_uses_result_names(r))
                || stmts_use_result_names(&f.body)
        }
        Stmt::ClassDef(c) => stmts_use_result_names(&c.body),
        Stmt::AnnAssign(a) => {
            expr_uses_result_names(&a.annotation)
                || a.value.as_ref().map_or(false, |v| expr_uses_result_names(v))
        }
        Stmt::Assign(a) => expr_uses_result_names(&a.value),
        Stmt::Return(r) => r.value.as_ref().map_or(false, |v| expr_uses_result_names(v)),
        Stmt::Expr(e) => expr_uses_result_names(&e.value),
        Stmt::If(i) => {
            stmts_use_result_names(&i.body) || stmts_use_result_names(&i.orelse)
        }
        _ => false,
    }
}

fn expr_uses_result_names(expr: &Expr<TextRange>) -> bool {
    match expr {
        Expr::Name(n) => matches!(n.id.as_str(), "Ok" | "Err" | "Result"),
        Expr::Call(c) => {
            expr_uses_result_names(&c.func)
                || c.args.iter().any(|a| expr_uses_result_names(a))
        }
        Expr::Subscript(s) => {
            expr_uses_result_names(&s.value) || expr_uses_result_names(&s.slice)
        }
        Expr::BinOp(b) => expr_uses_result_names(&b.left) || expr_uses_result_names(&b.right),
        Expr::Tuple(t) => t.elts.iter().any(|e| expr_uses_result_names(e)),
        Expr::Attribute(a) => expr_uses_result_names(&a.value),
        _ => false,
    }
}

/// Return `true` if `body` already contains `from typhon_runtime import ...`.
fn has_typhon_runtime_import(body: &[Stmt<TextRange>]) -> bool {
    body.iter().any(|stmt| match stmt {
        Stmt::ImportFrom(imp) => imp.module.as_deref() == Some("typhon_runtime"),
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

    let final_body = if transformed_classes && !has_dataclasses_import(&m.body) {
        let insert_at = import_insert_pos(&new_body);
        let mut body = new_body;
        body.insert(insert_at, make_dataclasses_import());
        body
    } else {
        new_body
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
            let needs_decorator = !has_dataclass_decorator(&c.decorator_list);
            let (new_body, body_transformed) = desugar_stmts(&c.body);
            let mut new_class = c.clone();
            new_class.body = new_body;
            if needs_decorator {
                new_class.decorator_list.insert(0, make_dataclasses_dot_dataclass_call());
            }
            (Stmt::ClassDef(new_class), needs_decorator || body_transformed)
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

/// Return `true` if the decorator list already contains any recognized form of
/// the dataclass decorator:
/// - `@dataclass`          (bare name, from-import style)
/// - `@dataclass(...)`     (call, from-import style)
/// - `@dataclasses.dataclass`
/// - `@dataclasses.dataclass(...)`
fn has_dataclass_decorator(decorators: &[rustpython_ast::Expr<TextRange>]) -> bool {
    decorators.iter().any(|d| is_dataclass_expr(d))
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
        // No second import injected.
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
}
