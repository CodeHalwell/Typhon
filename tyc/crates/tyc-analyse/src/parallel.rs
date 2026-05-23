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
    ExprContext, ExprDictComp, ExprLambda, ExprListComp, ExprName, ExprSet, ExprSetComp,
    ExprStarred, ExprTuple, ModModule, Parameter, ParameterWithDefault, Parameters, Stmt,
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
///
/// `min_size` is the minimum *statically-known* iterable length below
/// which the rewrite is suppressed (matches `[strictness]
/// parallel-min-size` in `typhon.toml`).  When the iterable is a literal
/// list/tuple shorter than `min_size`, the comprehension is left alone
/// so the thread-pool overhead does not exceed the work.  When the
/// iterable's size cannot be inferred (e.g. it is an arbitrary call or
/// bound name), the threshold is treated as zero — the user opted into
/// `auto-parallel` and we honour that for unknown sizes.
pub fn rewrite_parallel_comprehensions(
    module: &mut ModModule,
    pure_callees: &HashSet<String>,
    min_size: u64,
) -> ParallelStats {
    let mut stats = ParallelStats::default();
    let ctx = RewriteCtx {
        pure: pure_callees,
        min_size,
    };
    for stmt in &mut module.body {
        rewrite_stmt(stmt, &ctx, &mut stats);
    }
    stats
}

struct RewriteCtx<'a> {
    pure: &'a HashSet<String>,
    min_size: u64,
}

fn rewrite_stmt(stmt: &mut Stmt, ctx: &RewriteCtx<'_>, stats: &mut ParallelStats) {
    match stmt {
        Stmt::FunctionDef(f) => {
            for s in &mut f.body {
                rewrite_stmt(s, ctx, stats);
            }
        }
        Stmt::ClassDef(c) => {
            for s in &mut c.body {
                rewrite_stmt(s, ctx, stats);
            }
        }
        Stmt::Assign(a) => {
            rewrite_expr(&mut a.value, ctx, stats);
        }
        Stmt::AnnAssign(a) => {
            if let Some(v) = a.value.as_mut() {
                rewrite_expr(v, ctx, stats);
            }
        }
        Stmt::AugAssign(a) => {
            rewrite_expr(&mut a.value, ctx, stats);
        }
        Stmt::Expr(e) => {
            rewrite_expr(&mut e.value, ctx, stats);
        }
        Stmt::Return(r) => {
            if let Some(v) = r.value.as_mut() {
                rewrite_expr(v, ctx, stats);
            }
        }
        Stmt::If(i) => {
            rewrite_expr(&mut i.test, ctx, stats);
            for s in &mut i.body {
                rewrite_stmt(s, ctx, stats);
            }
            for clause in &mut i.elif_else_clauses {
                if let Some(test) = clause.test.as_mut() {
                    rewrite_expr(test, ctx, stats);
                }
                for s in &mut clause.body {
                    rewrite_stmt(s, ctx, stats);
                }
            }
        }
        Stmt::While(w) => {
            rewrite_expr(&mut w.test, ctx, stats);
            for s in &mut w.body {
                rewrite_stmt(s, ctx, stats);
            }
        }
        Stmt::For(f) => {
            rewrite_expr(&mut f.iter, ctx, stats);
            for s in &mut f.body {
                rewrite_stmt(s, ctx, stats);
            }
        }
        Stmt::With(w) => {
            for s in &mut w.body {
                rewrite_stmt(s, ctx, stats);
            }
        }
        _ => {}
    }
}

fn rewrite_expr(expr: &mut Expr, ctx: &RewriteCtx<'_>, stats: &mut ParallelStats) {
    // Depth-first: a nested comprehension is rewritten before the outer
    // one sees it, so a `[f(x) for x in g(...)]` still triggers when the
    // iterable is itself complex.
    match expr {
        Expr::ListComp(lc) => {
            for gen in &mut lc.generators {
                rewrite_expr(&mut gen.iter, ctx, stats);
                for f in &mut gen.ifs {
                    rewrite_expr(f, ctx, stats);
                }
            }
            rewrite_expr(&mut lc.elt, ctx, stats);
            if let Some(rewritten) = try_rewrite_listcomp(lc, ctx) {
                *expr = rewritten;
                stats.rewrites += 1;
            }
        }
        Expr::SetComp(sc) => {
            for gen in &mut sc.generators {
                rewrite_expr(&mut gen.iter, ctx, stats);
                for f in &mut gen.ifs {
                    rewrite_expr(f, ctx, stats);
                }
            }
            rewrite_expr(&mut sc.elt, ctx, stats);
            if let Some(rewritten) = try_rewrite_setcomp(sc, ctx) {
                *expr = rewritten;
                stats.rewrites += 1;
            }
        }
        Expr::DictComp(dc) => {
            for gen in &mut dc.generators {
                rewrite_expr(&mut gen.iter, ctx, stats);
                for f in &mut gen.ifs {
                    rewrite_expr(f, ctx, stats);
                }
            }
            if let Some(ref mut key) = dc.key {
                rewrite_expr(key, ctx, stats);
            }
            rewrite_expr(&mut dc.value, ctx, stats);
            if let Some(rewritten) = try_rewrite_dictcomp(dc, ctx) {
                *expr = rewritten;
                stats.rewrites += 1;
            }
        }
        Expr::Call(c) => {
            rewrite_expr(&mut c.func, ctx, stats);
            for arg in &mut c.arguments.args {
                rewrite_expr(arg, ctx, stats);
            }
        }
        Expr::Tuple(t) => {
            for e in &mut t.elts {
                rewrite_expr(e, ctx, stats);
            }
        }
        Expr::List(l) => {
            for e in &mut l.elts {
                rewrite_expr(e, ctx, stats);
            }
        }
        _ => {}
    }
}

/// Attempt the rewrite on a single list comprehension. Returns `None` when
/// the shape does not match the conservative template documented at the
/// module level; the caller leaves the original expression in place.
fn try_rewrite_listcomp(lc: &ExprListComp, ctx: &RewriteCtx<'_>) -> Option<Expr> {
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
    if !ctx.pure.contains(&callee_name) {
        return None;
    }
    // Honour `[strictness] parallel-min-size`: suppress the rewrite when
    // the iterable is a literal list/tuple shorter than the configured
    // threshold.  Unknown sizes (an arbitrary call, name reference, …)
    // fall through — the user opted into auto-parallel and we trust the
    // shape will be worth parallelising at runtime.
    if let Some(literal_len) = literal_iter_len(&gen.iter) {
        if literal_len < ctx.min_size {
            return None;
        }
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

/// Attempt the rewrite on a set comprehension. Same eligibility rules
/// as the list-comp path: single generator, no filter, single bare-name
/// pure call. The result wraps the parallel map in a `{*<map_call>}`
/// set-display literal — the star-unpack form is preferred over
/// `set(map_call)` because `{...}` syntax does not depend on the
/// `set` name being unshadowed in the caller's scope (a user-level
/// `set = my_thing` rebind is rare but legal Python; the set literal
/// always resolves to the language-level set type).
fn try_rewrite_setcomp(sc: &ExprSetComp, ctx: &RewriteCtx<'_>) -> Option<Expr> {
    let (lambda_param, iter, elt_lambda) =
        analyse_comprehension(&sc.generators, &sc.elt, ctx, sc.range)?;
    let map_call = build_map_pure_call(sc.range, lambda_param, iter, elt_lambda);
    let starred = Expr::Starred(ExprStarred {
        range: sc.range,
        node_index: AtomicNodeIndex::NONE,
        value: Box::new(map_call),
        ctx: ExprContext::Load,
    });
    Some(Expr::Set(ExprSet {
        range: sc.range,
        node_index: AtomicNodeIndex::NONE,
        elts: vec![starred],
    }))
}

/// Attempt the rewrite on a dict comprehension. The eligibility rules
/// are similar to list/set comprehensions, but must handle both key and
/// value expressions. Only one of `key` or `value` may be a pure call;
/// the other must be a simple name or literal. The result wraps the
/// parallel map in `dict(map_pure(...))` — we use the `dict()` builtin
/// rather than a dict-display literal with unpacking because the lambda
/// must return key-value tuples, and `{**(k, v) for ...}` is invalid syntax.
fn try_rewrite_dictcomp(dc: &ExprDictComp, ctx: &RewriteCtx<'_>) -> Option<Expr> {
    // Single generator, no filters, no async
    if dc.generators.len() != 1 {
        return None;
    }
    let gen = &dc.generators[0];
    if gen.is_async || !gen.ifs.is_empty() {
        return None;
    }
    let target_name = match &gen.target {
        Expr::Name(n) => n.id.as_str().to_owned(),
        _ => return None,
    };

    // Check min-size threshold
    if let Some(literal_len) = literal_iter_len(&gen.iter) {
        if literal_len < ctx.min_size {
            return None;
        }
    }

    // Determine which of key/value contains the pure call
    let key = dc.key.as_ref()?;
    let value = dc.value.as_ref();

    // Helper to check if an expression is a simple name or literal
    let is_simple = |e: &Expr| matches!(e, Expr::Name(_) | Expr::NumberLiteral(_) | Expr::StringLiteral(_) | Expr::BooleanLiteral(_) | Expr::NoneLiteral(_));

    // Helper to check if an expression is a pure call on the target
    let is_pure_call_on_target = |e: &Expr| -> Option<String> {
        let call = match e {
            Expr::Call(c) => c,
            _ => return None,
        };
        let callee_name = match call.func.as_ref() {
            Expr::Name(n) => n.id.as_str().to_owned(),
            _ => return None,
        };
        if !ctx.pure.contains(&callee_name) {
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
        Some(callee_name)
    };

    // Try to determine the rewrite pattern:
    // Case 1: {key_expr: f(x) for x in xs} where key_expr is simple
    // Case 2: {f(x): value_expr for x in xs} where value_expr is simple
    let (lambda_body_key, lambda_body_value) = if is_simple(key) {
        // value must be the pure call
        if is_pure_call_on_target(value).is_some() {
            // Lambda returns (key_expr, f(x))
            (key.as_ref().clone(), value.clone())
        } else {
            return None;
        }
    } else if is_simple(value) {
        // key must be the pure call
        if is_pure_call_on_target(key).is_some() {
            // Lambda returns (f(x), value_expr)
            (key.as_ref().clone(), value.clone())
        } else {
            return None;
        }
    } else {
        // Neither pattern matches
        return None;
    };

    // Build lambda that returns a tuple (key, value)
    let tuple = Expr::Tuple(ExprTuple {
        range: dc.range,
        node_index: AtomicNodeIndex::NONE,
        elts: vec![lambda_body_key, lambda_body_value],
        ctx: ExprContext::Load,
        parenthesized: false,
    });

    let map_call = build_map_pure_call(dc.range, target_name, gen.iter.clone(), tuple);

    // Wrap in dict(...) call
    let dict_name = Expr::Name(ExprName {
        range: dc.range,
        node_index: AtomicNodeIndex::NONE,
        id: Name::new_static("dict"),
        ctx: ExprContext::Load,
    });

    Some(Expr::Call(ExprCall {
        range: dc.range,
        node_index: AtomicNodeIndex::NONE,
        func: Box::new(dict_name),
        arguments: Arguments {
            range: dc.range,
            node_index: AtomicNodeIndex::NONE,
            args: vec![map_call].into_boxed_slice(),
            keywords: Vec::new().into_boxed_slice(),
        },
    }))
}

/// Shared eligibility check for list / set comprehensions.
///
/// Returns `(target_name, iterable_expr, body_expr)` on a match. Both
/// `try_rewrite_listcomp` and `try_rewrite_setcomp` consume the same
/// shape, so factoring this prevents drift between them.
fn analyse_comprehension(
    generators: &[Comprehension],
    elt: &Expr,
    ctx: &RewriteCtx<'_>,
    _range: TextRange,
) -> Option<(String, Expr, Expr)> {
    if generators.len() != 1 {
        return None;
    }
    let gen = &generators[0];
    if gen.is_async || !gen.ifs.is_empty() {
        return None;
    }
    let target_name = match &gen.target {
        Expr::Name(n) => n.id.as_str().to_owned(),
        _ => return None,
    };
    let call = match elt {
        Expr::Call(c) => c,
        _ => return None,
    };
    let callee_name = match call.func.as_ref() {
        Expr::Name(n) => n.id.as_str().to_owned(),
        _ => return None,
    };
    if !ctx.pure.contains(&callee_name) {
        return None;
    }
    if let Some(literal_len) = literal_iter_len(&gen.iter) {
        if literal_len < ctx.min_size {
            return None;
        }
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
    Some((target_name, gen.iter.clone(), elt.clone()))
}

/// Synthesise `typhon_runtime.parallel.map_pure(lambda <param>: <body>, <iter>)`.
fn build_map_pure_call(range: TextRange, lambda_param: String, iter: Expr, body: Expr) -> Expr {
    let lambda = ExprLambda {
        range,
        node_index: AtomicNodeIndex::NONE,
        parameters: Some(Box::new(Parameters {
            range,
            node_index: AtomicNodeIndex::NONE,
            posonlyargs: Vec::new(),
            args: vec![ParameterWithDefault {
                range,
                node_index: AtomicNodeIndex::NONE,
                parameter: Parameter {
                    range,
                    node_index: AtomicNodeIndex::NONE,
                    name: ident(&lambda_param, range),
                    annotation: None,
                },
                default: None,
            }],
            vararg: None,
            kwonlyargs: Vec::new(),
            kwarg: None,
        })),
        body: Box::new(body),
    };
    let map_pure = Expr::Attribute(ExprAttribute {
        range,
        node_index: AtomicNodeIndex::NONE,
        value: Box::new(Expr::Attribute(ExprAttribute {
            range,
            node_index: AtomicNodeIndex::NONE,
            value: Box::new(Expr::Name(ExprName {
                range,
                node_index: AtomicNodeIndex::NONE,
                id: Name::new("typhon_runtime"),
                ctx: ExprContext::Load,
            })),
            attr: ident("parallel", range),
            ctx: ExprContext::Load,
        })),
        attr: ident("map_pure", range),
        ctx: ExprContext::Load,
    });
    Expr::Call(ExprCall {
        range,
        node_index: AtomicNodeIndex::NONE,
        func: Box::new(map_pure),
        arguments: Arguments {
            range,
            node_index: AtomicNodeIndex::NONE,
            args: vec![Expr::Lambda(lambda), iter].into_boxed_slice(),
            keywords: Vec::new().into_boxed_slice(),
        },
    })
}

fn ident(name: &str, range: TextRange) -> ruff_python_ast::Identifier {
    ruff_python_ast::Identifier {
        range,
        node_index: AtomicNodeIndex::NONE,
        id: Name::new(name),
    }
}

/// Statically-known length for a literal iterable expression.
///
/// Recognises `[a, b, c]` and `(a, b, c)` shapes and returns their element
/// count.  All other forms (`range(n)`, a bare name, a function call)
/// return `None`, signalling "unknown size" so the auto-parallel pass
/// proceeds as before.
fn literal_iter_len(expr: &Expr) -> Option<u64> {
    match expr {
        Expr::List(l) => Some(l.elts.len() as u64),
        Expr::Tuple(t) => Some(t.elts.len() as u64),
        Expr::Set(s) => Some(s.elts.len() as u64),
        _ => None,
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
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
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
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(stats.rewrites, 0);
    }

    #[test]
    fn leaves_filtered_comprehension_alone() {
        let src = "ys = [f(x) for x in xs if x > 0]\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(stats.rewrites, 0, "filter must veto the rewrite");
    }

    #[test]
    fn leaves_nested_comprehension_alone() {
        let src = "ys = [f(x) for row in rows for x in row]\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(stats.rewrites, 0);
    }

    #[test]
    fn leaves_multi_arg_call_alone() {
        let src = "ys = [f(x, 1) for x in xs]\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(stats.rewrites, 0);
    }

    #[test]
    fn rewrites_inside_function_body() {
        let src = "def run() -> list[int]:\n    return [f(x) for x in xs]\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(stats.rewrites, 1);
    }

    #[test]
    fn min_size_threshold_suppresses_short_literal_iters() {
        // `[1, 2, 3]` is statically size 3; threshold 64 should suppress.
        let src = "ys = [f(x) for x in [1, 2, 3]]\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 64);
        assert_eq!(
            stats.rewrites, 0,
            "literal iter shorter than threshold must not rewrite"
        );
    }

    #[test]
    fn min_size_threshold_passes_long_literal_iters() {
        // 64 elements ≥ threshold of 64 → rewrite.
        let elts = (0..64)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let src = format!("ys = [f(x) for x in [{elts}]]\n");
        let mut m = parse(&src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 64);
        assert_eq!(stats.rewrites, 1);
    }

    #[test]
    fn min_size_threshold_passes_unknown_size_iters() {
        // An arbitrary call has no statically-known size — proceed.
        let src = "ys = [f(x) for x in fetch()]\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 64);
        assert_eq!(
            stats.rewrites, 1,
            "unknown iter size should fall through the threshold"
        );
    }

    // ── set comprehensions ─────────────────────────────────────────────────

    #[test]
    fn rewrites_pure_setcomp_uses_set_literal_to_avoid_shadowing() {
        let src = "ys = {f(x) for x in xs}\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(stats.rewrites, 1, "set comprehension should rewrite");
        let emit = tyc_emit::emit_python(&m);
        assert!(
            emit.contains("typhon_runtime.parallel.map_pure"),
            "expected map_pure rewrite; got: {emit}"
        );
        // Emits `{*typhon_runtime.parallel.map_pure(...)}` rather
        // than `set(...)` so a user-level `set = my_thing` rebind
        // doesn't break the rewrite.
        assert!(
            emit.contains("{*typhon_runtime"),
            "expected `{{*…}}` set-literal wrapper, not `set(...)`; got: {emit}"
        );
        assert!(
            !emit.contains("set("),
            "should not depend on the `set` name; got: {emit}"
        );
    }

    #[test]
    fn leaves_filtered_setcomp_alone() {
        let src = "ys = {f(x) for x in xs if x > 0}\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(stats.rewrites, 0);
    }

    #[test]
    fn leaves_impure_setcomp_alone() {
        let src = "ys = {g(x) for x in xs}\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(stats.rewrites, 0);
    }
}
