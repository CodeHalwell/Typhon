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
    AtomicNodeIndex, Decorator, Expr, ExprEllipsisLiteral, ModModule, Stmt, StmtExpr,
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
    prune_unused_dataclasses_import(&mut out);
    out
}

fn stub_stmt(stmt: &Stmt) -> Option<Stmt> {
    match stmt {
        Stmt::Import(_) | Stmt::ImportFrom(_) | Stmt::TypeAlias(_) => Some(stmt.clone()),
        Stmt::AnnAssign(a) => {
            // Drop any `= default` — stubs annotate types, they don't
            // carry implementation-supplied defaults.
            let mut new_a = a.clone();
            new_a.value = None;
            Some(Stmt::AnnAssign(new_a))
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
            // Strip `@dataclasses.dataclass(...)` / `@dataclass(...)`
            // decorators — they're an implementation choice that doesn't
            // belong on the stub surface. Other decorators
            // (`@functools.cached_property`, user-authored ones) stay
            // because they're part of the consumer-visible API.
            new_c.decorator_list.retain(|d| !is_dataclass_decorator(d));
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

fn is_dataclass_decorator(d: &Decorator) -> bool {
    // Accept the call form `@dataclasses.dataclass(...)` /
    // `@dataclass(...)` and the bare-reference form
    // `@dataclasses.dataclass` / `@dataclass`.
    let target = match &d.expression {
        Expr::Call(call) => call.func.as_ref(),
        other => other,
    };
    match target {
        Expr::Attribute(attr) => {
            let head_is_dataclasses = matches!(
                attr.value.as_ref(),
                Expr::Name(n) if n.id.as_str() == "dataclasses"
            );
            head_is_dataclasses && attr.attr.as_str() == "dataclass"
        }
        Expr::Name(n) => n.id.as_str() == "dataclass",
        _ => false,
    }
}

fn import_uses_dataclasses(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Import(imp) => imp.names.iter().any(|a| a.name.as_str() == "dataclasses"),
        _ => false,
    }
}

fn stmt_references_dataclasses(stmt: &Stmt) -> bool {
    struct Walker {
        found: bool,
    }
    impl<'a> ruff_python_ast::visitor::source_order::SourceOrderVisitor<'a> for Walker {
        fn visit_expr(&mut self, e: &'a Expr) {
            if self.found {
                return;
            }
            if let Expr::Name(n) = e {
                if n.id.as_str() == "dataclasses" {
                    self.found = true;
                    return;
                }
            }
            ruff_python_ast::visitor::source_order::walk_expr(self, e);
        }
    }
    let mut w = Walker { found: false };
    use ruff_python_ast::visitor::source_order::SourceOrderVisitor;
    w.visit_stmt(stmt);
    w.found
}

fn prune_unused_dataclasses_import(body: &mut Vec<Stmt>) {
    let referenced = body
        .iter()
        .any(|s| !import_uses_dataclasses(s) && stmt_references_dataclasses(s));
    if !referenced {
        body.retain(|s| !import_uses_dataclasses(s));
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

    #[test]
    fn dataclass_decorator_stripped() {
        let s =
            stub("import dataclasses\n@dataclasses.dataclass(slots=True)\nclass C:\n    x: int\n");
        assert!(!s.contains("@dataclasses.dataclass"), "got: {s}");
        assert!(!s.contains("import dataclasses"), "got: {s}");
        assert!(s.contains("class C:"), "got: {s}");
        assert!(s.contains("x: int"), "got: {s}");
    }

    #[test]
    fn defaulted_field_default_dropped() {
        let s = stub("class C:\n    x: int = 5\n");
        assert!(s.contains("x: int"), "got: {s}");
        assert!(!s.contains("= 5"), "default must be dropped: {s}");
    }

    #[test]
    fn dataclasses_import_kept_when_referenced_elsewhere() {
        let s = stub(
            "import dataclasses\nFACTORY: dataclasses.Field = dataclasses.field()\nclass C:\n    x: int\n",
        );
        assert!(s.contains("import dataclasses"), "got: {s}");
    }
}
