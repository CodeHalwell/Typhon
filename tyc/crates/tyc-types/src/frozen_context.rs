//! Exception boundaries for newly checked immutable-container operations.
//! Reads syntax only; it does not change checker flow state or narrowing.
use super::Checker;
use ruff_python_ast::{
    visitor::{self, Visitor},
    ExceptHandler, Expr, Stmt,
};
use ruff_text_size::Ranged;

pub(super) fn failure_is_caught(c: &Checker, expr: &Expr, error: &str) -> bool {
    let Some(module) = c.module else { return false };
    fn catches(expr: &Expr, error: &str) -> bool {
        match expr {
            Expr::Name(n) => {
                matches!(n.id.as_str(), "Exception" | "BaseException") || n.id.as_str() == error
            }
            Expr::Tuple(t) => t.elts.iter().any(|e| catches(e, error)),
            _ => false,
        }
    }
    struct Scan<'s> {
        target: ruff_text_size::TextRange,
        error: &'s str,
        protected: bool,
        found: bool,
    }
    impl<'a> Visitor<'a> for Scan<'_> {
        fn visit_expr(&mut self, expr: &'a Expr) {
            if expr.range() == self.target && self.protected {
                self.found = true;
            }
            if self.found {
                return;
            }
            // A lambda's defaults run where it is created, but its body runs
            // later — outside any handler around the lambda expression —
            // just like a nested `def`.
            if let Expr::Lambda(lambda) = expr {
                if let Some(parameters) = &lambda.parameters {
                    self.visit_parameters(parameters);
                }
                let prior = self.protected;
                self.protected = false;
                self.visit_expr(&lambda.body);
                self.protected = prior;
                return;
            }
            visitor::walk_expr(self, expr);
        }
        fn visit_stmt(&mut self, stmt: &'a Stmt) {
            if self.found {
                return;
            }
            match stmt {
                Stmt::FunctionDef(_) | Stmt::ClassDef(_) => {
                    let prior = self.protected;
                    self.protected = false;
                    visitor::walk_stmt(self, stmt);
                    self.protected = prior;
                }
                Stmt::Try(t) => {
                    let prior = self.protected;
                    self.protected |= t.handlers.iter().any(|handler| {
                        let ExceptHandler::ExceptHandler(handler) = handler;
                        handler
                            .type_
                            .as_deref()
                            .is_none_or(|e| catches(e, self.error))
                            && !handler.body.iter().any(|s| matches!(s, Stmt::Raise(_)))
                    });
                    for stmt in &t.body {
                        self.visit_stmt(stmt);
                    }
                    self.protected = prior;
                    for handler in &t.handlers {
                        let ExceptHandler::ExceptHandler(handler) = handler;
                        for stmt in &handler.body {
                            self.visit_stmt(stmt);
                        }
                    }
                    for stmt in t.orelse.iter().chain(&t.finalbody) {
                        self.visit_stmt(stmt);
                    }
                }
                _ => visitor::walk_stmt(self, stmt),
            }
        }
    }
    if c.local_classes.contains(error) {
        return false;
    }
    let mut scan = Scan {
        target: expr.range(),
        error,
        protected: false,
        found: false,
    };
    for stmt in &module.body {
        scan.visit_stmt(stmt);
    }
    scan.found
}
