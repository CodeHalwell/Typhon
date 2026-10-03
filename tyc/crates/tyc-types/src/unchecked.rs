//! Debug observability for unchecked annotation, return and member sites.
use super::Type;
use ruff_python_ast::{
    visitor::{self, Visitor},
    Expr, ModModule, Stmt,
};
use ruff_text_size::Ranged;
use std::collections::HashMap;

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Counts {
    pub annotated: usize,
    pub returns: usize,
    pub members: usize,
}
fn unknown(ty: &Type) -> bool {
    match ty {
        Type::Unknown => true,
        Type::Generic(_, args) | Type::Union(args) => args.iter().any(unknown),
        Type::Function { ret, .. } => unknown(ret),
        _ => false,
    }
}
pub(super) fn counts(module: &ModModule, facts: &HashMap<(usize, usize), Type>) -> Counts {
    struct Scan<'a> {
        facts: &'a HashMap<(usize, usize), Type>,
        counts: Counts,
        annotated_return: bool,
    }
    impl Scan<'_> {
        fn unchecked(&self, expr: &Expr) -> bool {
            self.facts
                .get(&(
                    expr.range().start().to_usize(),
                    expr.range().end().to_usize(),
                ))
                .is_some_and(unknown)
        }
    }
    impl<'a> Visitor<'a> for Scan<'_> {
        fn visit_stmt(&mut self, stmt: &'a Stmt) {
            match stmt {
                Stmt::AnnAssign(assign) => {
                    if assign.value.as_deref().is_some_and(|e| self.unchecked(e)) {
                        self.counts.annotated += 1;
                    }
                }
                Stmt::Return(ret) if self.annotated_return => {
                    if ret.value.as_deref().is_some_and(|e| self.unchecked(e)) {
                        self.counts.returns += 1;
                    }
                }
                Stmt::FunctionDef(function) => {
                    let previous = self.annotated_return;
                    self.annotated_return = function.returns.is_some();
                    visitor::walk_stmt(self, stmt);
                    self.annotated_return = previous;
                    return;
                }
                _ => {}
            }
            visitor::walk_stmt(self, stmt);
        }
        fn visit_expr(&mut self, expr: &'a Expr) {
            if let Expr::Attribute(attr) = expr {
                if self.unchecked(expr) || self.unchecked(&attr.value) {
                    self.counts.members += 1;
                }
            }
            visitor::walk_expr(self, expr);
        }
    }
    let mut scan = Scan {
        facts,
        counts: Counts::default(),
        annotated_return: false,
    };
    for stmt in &module.body {
        scan.visit_stmt(stmt);
    }
    scan.counts
}
pub(super) fn report(path: &str, module: &ModModule, facts: &HashMap<(usize, usize), Type>) {
    if std::env::var("TYC_REPORT_UNCHECKED").as_deref() != Ok("1") {
        return;
    }
    let counts = counts(module, facts);
    eprintln!(
        "tyc unchecked {path:?}: annotated={} return={} member={} total={}",
        counts.annotated,
        counts.returns,
        counts.members,
        counts.annotated + counts.returns + counts.members
    );
}
