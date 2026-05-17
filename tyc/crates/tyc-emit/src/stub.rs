//! Emit a PEP 561 `.pyi` stub from a desugared Typhon AST.
//!
//! Stubs differ from full source emission in three ways:
//!
//! 1. Function and method bodies are replaced with `...` — the stub records
//!    the signature only, not the implementation.
//! 2. Statements with no static type significance (assignments without
//!    annotations, expression statements, control flow) are dropped.
//! 3. Class bodies keep only their annotated fields and method signatures;
//!    `model_config = ConfigDict(...)` and similar configuration assignments
//!    are dropped because they don't affect the consumer's view of the API.
//!
//! The output is valid Python that type-checkers (mypy, pyright, ty, Pyrefly)
//! consume directly. The `.dty` source remains the authoritative document for
//! Typhon-internal use; `.pyi` is for the outside world.

use ruff_python_ast::{
    AtomicNodeIndex, Expr, ExprEllipsisLiteral, ModModule, Stmt, StmtExpr,
};
use ruff_text_size::TextRange;

use crate::printer::Emitter;

/// Emit `module` as a `.pyi` stub. Returns the rendered stub text.
pub fn emit_stub(module: &ModModule) -> String {
    let stub_body = strip_to_stubs(&module.body);
    let stub_mod = ModModule {
        range: module.range,
        node_index: AtomicNodeIndex::NONE,
        body: stub_body,
    };
    let mut emitter = Emitter::new();
    emitter.emit_mod(&stub_mod);
    emitter.finish()
}

fn strip_to_stubs(body: &[Stmt]) -> Vec<Stmt> {
    let mut out = Vec::with_capacity(body.len());
    for stmt in body {
        if let Some(stripped) = stub_stmt(stmt) {
            out.push(stripped);
        }
    }
    out
}

fn stub_stmt(stmt: &Stmt) -> Option<Stmt> {
    match stmt {
        // Pass-through statements that carry public-API information.
        Stmt::Import(_) | Stmt::ImportFrom(_) | Stmt::AnnAssign(_) | Stmt::TypeAlias(_) => {
            Some(stmt.clone())
        }
        // `StmtFunctionDef` now covers both sync and async functions via
        // `is_async`; the body-rewrite is identical in both cases.
        Stmt::FunctionDef(f) => {
            let mut new_f = f.clone();
            new_f.body = vec![ellipsis_stmt(f.range)];
            Some(Stmt::FunctionDef(new_f))
        }
        Stmt::ClassDef(c) => {
            let mut new_c = c.clone();
            let mut body = Vec::new();
            for s in &c.body {
                if let Some(kept) = stub_stmt(s) {
                    body.push(kept);
                }
            }
            if body.is_empty() {
                body.push(ellipsis_stmt(c.range));
            }
            new_c.body = body;
            Some(Stmt::ClassDef(new_c))
        }
        // Plain `Assign` is dropped — without an annotation we can't infer the
        // consumer-visible type, so it's not stub material.
        _ => None,
    }
}

fn ellipsis_stmt(range: TextRange) -> Stmt {
    Stmt::Expr(StmtExpr {
        range,
        node_index: AtomicNodeIndex::NONE,
        value: Box::new(Expr::EllipsisLiteral(ExprEllipsisLiteral {
            range,
            node_index: AtomicNodeIndex::NONE,
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tyc_syntax::parse_module;

    fn stub(src: &str) -> String {
        let parsed = parse_module(src).expect("parse failed");
        emit_stub(parsed.syntax())
    }

    #[test]
    fn function_body_becomes_ellipsis() {
        let s = stub("def add(a: int, b: int) -> int:\n    return a + b\n");
        assert!(s.contains("def add(a: int, b: int) -> int:"), "got: {s}");
        assert!(s.contains("..."), "body must be ...: {s}");
        assert!(
            !s.contains("return"),
            "body must not include implementation: {s}"
        );
    }

    #[test]
    fn class_keeps_annotated_fields_and_signatures() {
        let s = stub(
            "class User:\n    id: int\n    name: str\n    def hello(self) -> str:\n        return self.name\n",
        );
        assert!(s.contains("class User:"), "got: {s}");
        assert!(s.contains("id: int"), "got: {s}");
        assert!(s.contains("name: str"), "got: {s}");
        assert!(s.contains("def hello(self) -> str:"), "got: {s}");
        assert!(s.contains("..."), "method body must be ...: {s}");
    }

    #[test]
    fn empty_class_body_emits_ellipsis() {
        let s = stub("class Tag:\n    pass\n");
        assert!(s.contains("class Tag:"), "got: {s}");
        assert!(s.contains("..."), "got: {s}");
    }

    #[test]
    fn imports_pass_through() {
        let s = stub("from typing import Protocol\n\ndef f() -> int: ...\n");
        assert!(s.contains("from typing import Protocol"), "got: {s}");
    }

    #[test]
    fn type_alias_passes_through() {
        let s = stub("type Vec = list[int]\n");
        assert!(s.contains("type Vec = list[int]"), "got: {s}");
    }

    #[test]
    fn plain_assignment_is_dropped() {
        let s = stub("x = 1\n");
        assert!(
            !s.contains("x = 1"),
            "plain assigns are not stub material: {s}"
        );
    }

    #[test]
    fn ann_assign_is_kept() {
        let s = stub("PORT: int = 8080\n");
        assert!(s.contains("PORT: int"), "annotated assigns are stubs: {s}");
    }

    #[test]
    fn async_function_body_becomes_ellipsis() {
        let s = stub("async def fetch(url: str) -> str:\n    return url\n");
        assert!(s.contains("async def fetch(url: str) -> str:"), "got: {s}");
        assert!(s.contains("..."), "got: {s}");
    }
}
