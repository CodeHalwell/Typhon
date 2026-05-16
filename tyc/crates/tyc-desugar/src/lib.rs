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
    text_size::TextRange, Constant, ExprCall, ExprConstant, ExprContext, ExprName, Identifier,
    Mod, ModModule, Stmt, StmtImport,
};

// ── public API ───────────────────────────────────────────────────────────────

/// Desugar a Typhon module AST into a plain Python AST.
///
/// Currently performs one transformation: every `class` definition that does
/// not already carry a `@dataclass` or `@dataclasses.dataclass` decorator gets
/// `@dataclasses.dataclass(slots=True)` prepended to its decorator list, and
/// `import dataclasses` is injected after any leading docstring / future
/// imports when at least one class was transformed. The transformation is
/// recursive — nested classes inside functions or other classes are processed.
pub fn desugar_module(module: &Mod<TextRange>) -> Mod<TextRange> {
    match module {
        Mod::Module(m) => Mod::Module(desugar_mod_module(m)),
        other => other.clone(),
    }
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
        let desugared = desugar_module(&m);
        emit(&desugared)
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
}
