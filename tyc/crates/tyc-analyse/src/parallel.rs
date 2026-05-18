//! Loop / comprehension parallelisation (Phase 4+).
//!
//! Detects list-comprehensions whose element expression is a pure call and
//! rewrites them into a thread-pool map.  The output references
//! `typhon_runtime.parallel.map_pure`, a helper generated alongside the
//! rest of the runtime (`tasks.py`, `lazy.py`).  Combine with the
//! `[python] free-threaded` config flag to get real parallelism — on stock
//! CPython the GIL serialises the workers but correctness is preserved.
//!
//! The transform is intentionally conservative.  A comprehension qualifies
//! only when every condition holds:
//!
//!   1. **Shape.** Single-target single-generator list comprehension —
//!      `[expr for x in iter]`.  No filters, no nesting, no `async for`,
//!      and only one bound name.
//!   2. **Element.** The body is a call expression whose callee is a
//!      bare name (so we can recover it as a `lambda` of one argument)
//!      and whose only argument list is `[Name(target)]`.  In practice
//!      that covers the common `[f(x) for x in xs]` form.
//!   3. **Purity.** The callee must appear in the supplied set of
//!      pure-function names — typically every function the analyser
//!      already proved pure under the six-condition rule.
//!
//! The rewrite preserves the comprehension's location and the order of
//! the bound results: `map_pure` returns results in input order.  Code
//! that relied on the laziness of a generator expression is unaffected
//! because we only touch list-comprehensions.

use std::collections::HashSet;

use ruff_python_ast::{
    name::Name, Arguments, AtomicNodeIndex, Comprehension, Expr, ExprAttribute, ExprCall,
    ExprContext, ExprLambda, ExprListComp, ExprName, ModModule, Parameter, ParameterWithDefault,
    Parameters, Stmt,
};
use ruff_text_size::TextRange;

/// Summary of what the pass rewrote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParallelStats {
    /// Number of comprehensions converted to `map_pure` calls.
    pub rewrites: usize,
}

/// Walk `module` and rewrite eligible list comprehensions in place.
///
/// `pure_callees` is the set of bare-name functions that the purity pass
/// already proved safe to invoke concurrently.  When the set is empty the
/// pass is effectively a no-op.
pub fn rewrite_parallel_comprehensions(
    module: &mut ModModule,
    pure_callees: &HashSet<String>,
) -> ParallelStats {
    let mut stats = ParallelStats::default();
    for stmt in &mut module.body {
        rewrite_stmt(stmt, pure_callees, &mut stats);
    }
    stats
}

fn rewrite_stmt(stmt: &mut Stmt, pure: &HashSet<String>, stats: &mut ParallelStats) {
    match stmt {
        Stmt::FunctionDef(f) => {
            for s in &mut f.body {
                rewrite_stmt(s, pure, stats);
            }
        }
        Stmt::ClassDef(c) => {
            for s in &mut c.body {
                rewrite_stmt(s, pure, stats);
            }
        }
        Stmt::Assign(a) => {
            rewrite_expr(&mut a.value, pure, stats);
        }
        Stmt::AnnAssign(a) => {
            if let Some(v) = a.value.as_mut() {
                rewrite_expr(v, pure, stats);
            }
        }
        Stmt::AugAssign(a) => {
            rewrite_expr(&mut a.value, pure, stats);
        }
        Stmt::Expr(e) => {
            rewrite_expr(&mut e.value, pure, stats);
        }
        Stmt::Return(r) => {
            if let Some(v) = r.value.as_mut() {
                rewrite_expr(v, pure, stats);
            }
        }
        Stmt::If(i) => {
            rewrite_expr(&mut i.test, pure, stats);
            for s in &mut i.body {
                rewrite_stmt(s, pure, stats);
            }
            for clause in &mut i.elif_else_clauses {
                for s in &mut clause.body {
                    rewrite_stmt(s, pure, stats);
                }
            }
        }
        Stmt::While(w) => {
            rewrite_expr(&mut w.test, pure, stats);
            for s in &mut w.body {
                rewrite_stmt(s, pure, stats);
            }
        }
        Stmt::For(f) => {
            rewrite_expr(&mut f.iter, pure, stats);
            for s in &mut f.body {
                rewrite_stmt(s, pure, stats);
            }
        }
        Stmt::With(w) => {
            for s in &mut w.body {
                rewrite_stmt(s, pure, stats);
            }
        }
        _ => {}
    }
}

fn rewrite_expr(expr: &mut Expr, pure: &HashSet<String>, stats: &mut ParallelStats) {
    // Depth-first: a nested comprehension is rewritten before the outer
    // one sees it, so a `[f(x) for x in g(...)]` still triggers when the
    // iterable is itself complex.
    match expr {
        Expr::ListComp(lc) => {
            for gen in &mut lc.generators {
                rewrite_expr(&mut gen.iter, pure, stats);
                for f in &mut gen.ifs {
                    rewrite_expr(f, pure, stats);
                }
            }
            rewrite_expr(&mut lc.elt, pure, stats);
            if let Some(rewritten) = try_rewrite_listcomp(lc, pure) {
                *expr = rewritten;
                stats.rewrites += 1;
            }
        }
        Expr::Call(c) => {
            rewrite_expr(&mut c.func, pure, stats);
            for arg in &mut c.arguments.args {
                rewrite_expr(arg, pure, stats);
            }
        }
        Expr::Tuple(t) => {
            for e in &mut t.elts {
                rewrite_expr(e, pure, stats);
            }
        }
        Expr::List(l) => {
            for e in &mut l.elts {
                rewrite_expr(e, pure, stats);
            }
        }
        _ => {}
    }
}

/// Attempt the rewrite on a single list comprehension. Returns `None` when
/// the shape does not match the conservative template documented at the
/// module level; the caller leaves the original expression in place.
fn try_rewrite_listcomp(lc: &ExprListComp, pure: &HashSet<String>) -> Option<Expr> {
    if lc.generators.len() != 1 {
        return None;
    }
    let gen: &Comprehension = &lc.generators[0];
    if gen.is_async || !gen.ifs.is_empty() {
        return None;
    }
    let target_name = match &gen.target {
        Expr::Name(n) => n.id.as_str().to_owned(),
        _ => return None,
    };
    let call = match lc.elt.as_ref() {
        Expr::Call(c) => c,
        _ => return None,
    };
    let callee_name = match call.func.as_ref() {
        Expr::Name(n) => n.id.as_str().to_owned(),
        _ => return None,
    };
    if !pure.contains(&callee_name) {
        return None;
    }
    if !call.arguments.keywords.is_empty() || call.arguments.args.len() != 1 {
        return None;
    }
    let arg_is_target = matches!(
        call.arguments.args.first(),
        Some(Expr::Name(n)) if n.id.as_str() == target_name
    );
    if !arg_is_target {
        return None;
    }

    // Build a `lambda x: f(x)` wrapping the original element expression.
    let lambda = ExprLambda {
        range: lc.range,
        node_index: AtomicNodeIndex::NONE,
        parameters: Some(Box::new(Parameters {
            range: lc.range,
            node_index: AtomicNodeIndex::NONE,
            posonlyargs: Vec::new(),
            args: vec![ParameterWithDefault {
                range: lc.range,
                node_index: AtomicNodeIndex::NONE,
                parameter: Parameter {
                    range: lc.range,
                    node_index: AtomicNodeIndex::NONE,
                    name: ident(&target_name, lc.range),
                    annotation: None,
                },
                default: None,
            }],
            vararg: None,
            kwonlyargs: Vec::new(),
            kwarg: None,
        })),
        body: lc.elt.clone(),
    };
    let map_pure = Expr::Attribute(ExprAttribute {
        range: lc.range,
        node_index: AtomicNodeIndex::NONE,
        value: Box::new(Expr::Attribute(ExprAttribute {
            range: lc.range,
            node_index: AtomicNodeIndex::NONE,
            value: Box::new(Expr::Name(ExprName {
                range: lc.range,
                node_index: AtomicNodeIndex::NONE,
                id: Name::new("typhon_runtime"),
                ctx: ExprContext::Load,
            })),
            attr: ident("parallel", lc.range),
            ctx: ExprContext::Load,
        })),
        attr: ident("map_pure", lc.range),
        ctx: ExprContext::Load,
    });
    let call = Expr::Call(ExprCall {
        range: lc.range,
        node_index: AtomicNodeIndex::NONE,
        func: Box::new(map_pure),
        arguments: Arguments {
            range: lc.range,
            node_index: AtomicNodeIndex::NONE,
            args: vec![Expr::Lambda(lambda), gen.iter.clone()].into_boxed_slice(),
            keywords: Vec::new().into_boxed_slice(),
        },
    });
    Some(call)
}

fn ident(name: &str, range: TextRange) -> ruff_python_ast::Identifier {
    ruff_python_ast::Identifier {
        range,
        node_index: AtomicNodeIndex::NONE,
        id: Name::new(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> ModModule {
        tyc_syntax::parse_module(src).unwrap().into_syntax()
    }

    fn pure_set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn rewrites_pure_listcomp() {
        let src = "ys = [f(x) for x in xs]\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]));
        assert_eq!(stats.rewrites, 1);
        let emit = tyc_emit::emit_python(&m);
        assert!(
            emit.contains("typhon_runtime.parallel.map_pure"),
            "expected map_pure rewrite; got: {emit}"
        );
        assert!(
            emit.contains("lambda x"),
            "expected lambda wrapper; got: {emit}"
        );
    }

    #[test]
    fn leaves_impure_callee_alone() {
        let src = "ys = [g(x) for x in xs]\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]));
        assert_eq!(stats.rewrites, 0);
    }

    #[test]
    fn leaves_filtered_comprehension_alone() {
        let src = "ys = [f(x) for x in xs if x > 0]\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]));
        assert_eq!(stats.rewrites, 0, "filter must veto the rewrite");
    }

    #[test]
    fn leaves_nested_comprehension_alone() {
        let src = "ys = [f(x) for row in rows for x in row]\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]));
        assert_eq!(stats.rewrites, 0);
    }

    #[test]
    fn leaves_multi_arg_call_alone() {
        let src = "ys = [f(x, 1) for x in xs]\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]));
        assert_eq!(stats.rewrites, 0);
    }

    #[test]
    fn rewrites_inside_function_body() {
        let src = "def run() -> list[int]:\n    return [f(x) for x in xs]\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]));
        assert_eq!(stats.rewrites, 1);
    }
}
