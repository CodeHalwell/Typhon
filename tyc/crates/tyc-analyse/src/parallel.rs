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
//!   1. **Shape.** Single-target single-generator list / set / dict
//!      comprehension — `[expr for x in iter]` (optionally with `if`
//!      filters).  No nesting (multiple `for`), no `async for`, only one
//!      bound name.
//!   2. **Element.** The body is a *pure call* — a call whose callee is a
//!      bare name in the pure set and whose every argument is a
//!      [`is_pure_value_expr`] over the loop target (literals,
//!      provably-immutable captured names, arithmetic / comparison /
//!      boolean operators, and nested pure calls). This covers
//!      `[f(x) for x in xs]`, the multi-arg `[f(x, k) for x in xs]` (where
//!      `k` is a `let`-bound loop invariant), and the nested
//!      `[g(f(x)) for x in xs]`.  The target must appear at least once.
//!   3. **Filters.** Every `if COND` must itself be a [`is_pure_value_expr`],
//!      so the semantics-preserving rewrite runs the (pure) filter
//!      sequentially and the (pure) element map in parallel:
//!      `map_pure(lambda x: f(x), [x for x in xs if COND])`.
//!   4. **Purity.** Every callee reached must appear in the supplied set of
//!      pure-function names — typically every function the analyser
//!      already proved pure under the six-condition rule.
//!
//! The rewrite preserves the comprehension's location and the order of
//! the bound results: `map_pure` returns results in input order.  Code
//! that relied on the laziness of a generator expression is unaffected
//! because we only touch list / set / dict comprehensions.  Every widening
//! is semantics-preserving because the element, its captured arguments,
//! and the filters are all side-effect-free.

use std::collections::HashSet;

use ruff_python_ast::{
    name::Name, Arguments, AtomicNodeIndex, Comprehension, ExceptHandler, Expr, ExprAttribute,
    ExprCall, ExprContext, ExprDictComp, ExprLambda, ExprListComp, ExprName, ExprSet, ExprSetComp,
    ExprStarred, ExprTuple, ModModule, Mutability, Parameter, ParameterWithDefault, Parameters,
    Pattern, Stmt,
};
use ruff_text_size::{Ranged, TextRange};

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
    let captures = collect_capturable_names(module);
    let ctx = RewriteCtx {
        pure: pure_callees,
        min_size,
        captures: &captures,
    };
    for stmt in &mut module.body {
        rewrite_stmt(stmt, &ctx, &mut stats);
    }
    stats
}

/// Shared analysis context for the parallel comprehension rewrite and the
/// integer-reduction rewrite (`crate::reductions`). Carries the pure-callee
/// set, the min-size threshold, and the capturable-name set.
pub(crate) struct RewriteCtx<'a> {
    pub(crate) pure: &'a HashSet<String>,
    pub(crate) min_size: u64,
    /// Names proven safe to capture by reference in a parallel-map lambda:
    /// `let`-bound (explicit or module-level implicit) or a parameter, and
    /// never mutated anywhere in the module. See [`collect_capturable_names`].
    pub(crate) captures: &'a HashSet<String>,
}

impl<'a> RewriteCtx<'a> {
    /// Construct a context from its parts. Used by `crate::reductions`, which
    /// shares the pure-value grammar and capture analysis.
    pub(crate) fn new(
        pure: &'a HashSet<String>,
        min_size: u64,
        captures: &'a HashSet<String>,
    ) -> Self {
        Self {
            pure,
            min_size,
            captures,
        }
    }
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
///
/// The result is `typhon_runtime.parallel.map_pure(lambda x: <elt>, <src>)`
/// where `<src>` is the original iterable (or, when the comprehension has
/// pure `if` filters, a sequential filtering list comprehension
/// `[x for x in iter if COND]`).
fn try_rewrite_listcomp(lc: &ExprListComp, ctx: &RewriteCtx<'_>) -> Option<Expr> {
    let (param, source, body) = analyse_comprehension(&lc.generators, &lc.elt, ctx, lc.range)?;
    Some(build_map_pure_call(lc.range, param, source, body))
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
/// the other must be a simple name or literal. The result emits a nested
/// dict comprehension: `{k: v for (k, v) in map_pure(...)}`, avoiding
/// any dependency on the shadowable `dict` builtin.
fn try_rewrite_dictcomp(dc: &ExprDictComp, ctx: &RewriteCtx<'_>) -> Option<Expr> {
    // Single generator, no async. Pure `if` filters are handled below by
    // running them sequentially in the map's source list.
    if dc.generators.len() != 1 {
        return None;
    }
    let gen = &dc.generators[0];
    if gen.is_async {
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

    // Determine which of key/value contains the pure call.
    let key = dc.key.as_ref()?;
    let value = dc.value.as_ref();

    // Exactly one of key / value must be a parallelisable pure call over the
    // target; the other must be a pure value expression (no side effects, so
    // it's safe to compute inside the mapped lambda). This covers
    // `{x: f(x) for x in xs}`, `{f(x): x for x in xs}`, the multi-arg
    // `{x: f(x, k) for x in xs}`, and the nested `{x: g(f(x)) for x in xs}`.
    let key_is_pure_call = elt_is_parallelisable(key, &target_name, ctx);
    let value_is_pure_call = elt_is_parallelisable(value, &target_name, ctx);
    let (lambda_body_key, lambda_body_value) =
        if !key_is_pure_call && value_is_pure_call && is_pure_value_expr(key, &target_name, ctx) {
            // {simple_key: f(x) for x in xs}
            (key.as_ref().clone(), value.clone())
        } else if key_is_pure_call
            && !value_is_pure_call
            && is_pure_value_expr(value, &target_name, ctx)
        {
            // {f(x): simple_value for x in xs}
            (key.as_ref().clone(), value.clone())
        } else {
            // Neither side matches, or both are pure calls (ambiguous — a single
            // map can't fan out two independent parallel calls per element).
            return None;
        };

    // Every filter must be a provably-pure expression over the target.
    if !gen
        .ifs
        .iter()
        .all(|f| is_pure_value_expr(f, &target_name, ctx))
    {
        return None;
    }

    // Build lambda that returns a tuple (key, value)
    let tuple = Expr::Tuple(ExprTuple {
        range: dc.range,
        node_index: AtomicNodeIndex::NONE,
        elts: vec![lambda_body_key, lambda_body_value],
        ctx: ExprContext::Load,
        parenthesized: false,
    });

    let source = build_filtered_source(&target_name, &gen.iter, &gen.ifs, dc.range);
    let map_call = build_map_pure_call(dc.range, target_name, source, tuple);

    // Rewrite as a dict comprehension: {k: v for (k, v) in map_pure(...)}
    // This avoids depending on the shadowable `dict` builtin.
    let tuple_target = Expr::Tuple(ExprTuple {
        range: dc.range,
        node_index: AtomicNodeIndex::NONE,
        elts: vec![
            Expr::Name(ExprName {
                range: dc.range,
                node_index: AtomicNodeIndex::NONE,
                id: Name::new_static("_k"),
                ctx: ExprContext::Store,
            }),
            Expr::Name(ExprName {
                range: dc.range,
                node_index: AtomicNodeIndex::NONE,
                id: Name::new_static("_v"),
                ctx: ExprContext::Store,
            }),
        ],
        ctx: ExprContext::Store,
        parenthesized: true,
    });

    let key_ref = Expr::Name(ExprName {
        range: dc.range,
        node_index: AtomicNodeIndex::NONE,
        id: Name::new_static("_k"),
        ctx: ExprContext::Load,
    });
    let value_ref = Expr::Name(ExprName {
        range: dc.range,
        node_index: AtomicNodeIndex::NONE,
        id: Name::new_static("_v"),
        ctx: ExprContext::Load,
    });

    Some(Expr::DictComp(ExprDictComp {
        range: dc.range,
        node_index: AtomicNodeIndex::NONE,
        key: Some(Box::new(key_ref)),
        value: Box::new(value_ref),
        generators: vec![Comprehension {
            range: dc.range,
            node_index: AtomicNodeIndex::NONE,
            target: tuple_target,
            iter: map_call,
            ifs: vec![],
            is_async: false,
        }],
    }))
}

/// Shared eligibility check for list / set comprehensions.
///
/// Returns `(target_name, source_expr, body_expr)` on a match, where
/// `source_expr` is the iterable the parallel map should run over — the
/// original `iter`, or a sequential filtering list comprehension when the
/// comprehension carries pure `if` filters. Both `try_rewrite_listcomp` and
/// `try_rewrite_setcomp` consume the same shape, so factoring this prevents
/// drift between them.
fn analyse_comprehension(
    generators: &[Comprehension],
    elt: &Expr,
    ctx: &RewriteCtx<'_>,
    range: TextRange,
) -> Option<(String, Expr, Expr)> {
    if generators.len() != 1 {
        return None;
    }
    let gen = &generators[0];
    if gen.is_async {
        return None;
    }
    let target_name = match &gen.target {
        Expr::Name(n) => n.id.as_str().to_owned(),
        _ => return None,
    };
    // Honour `[strictness] parallel-min-size`: suppress the rewrite when the
    // iterable is a literal list/tuple/set shorter than the configured
    // threshold. A filter can only shrink the source further, so checking the
    // pre-filter iterable is a safe upper bound. Unknown sizes fall through —
    // the user opted into auto-parallel.
    if let Some(literal_len) = literal_iter_len(&gen.iter) {
        if literal_len < ctx.min_size {
            return None;
        }
    }
    // The element must be a parallelisable pure call over the loop target.
    if !elt_is_parallelisable(elt, &target_name, ctx) {
        return None;
    }
    // Every filter must be a provably-pure expression over the loop target so
    // that filter-then-map is observationally identical to the comprehension.
    if !gen
        .ifs
        .iter()
        .all(|f| is_pure_value_expr(f, &target_name, ctx))
    {
        return None;
    }
    let source = build_filtered_source(&target_name, &gen.iter, &gen.ifs, range);
    Some((target_name, source, elt.clone()))
}

/// Synthesise `typhon_runtime.parallel.map_pure(lambda <param>: <body>, <iter>)`.
pub(crate) fn build_map_pure_call(
    range: TextRange,
    lambda_param: String,
    iter: Expr,
    body: Expr,
) -> Expr {
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

// ── shared eligibility predicates ─────────────────────────────────────────────

/// True when `name` is referenced anywhere inside `expr`. Used to require the
/// comprehension target actually appears in a parallelised element, so we
/// never rewrite `[f(k) for x in xs]` — an element that ignores the bound
/// name gains nothing from a per-element parallel map. Only walks the
/// expression shapes [`is_pure_value_expr`] admits.
pub(crate) fn mentions_name(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::Name(n) => n.id.as_str() == name,
        Expr::Call(c) => {
            mentions_name(&c.func, name)
                || c.arguments.args.iter().any(|a| mentions_name(a, name))
                || c.arguments
                    .keywords
                    .iter()
                    .any(|k| mentions_name(&k.value, name))
        }
        Expr::BinOp(b) => mentions_name(&b.left, name) || mentions_name(&b.right, name),
        Expr::UnaryOp(u) => mentions_name(&u.operand, name),
        Expr::BoolOp(b) => b.values.iter().any(|e| mentions_name(e, name)),
        Expr::Compare(c) => {
            mentions_name(&c.left, name) || c.comparators.iter().any(|e| mentions_name(e, name))
        }
        Expr::Starred(s) => mentions_name(&s.value, name),
        _ => false,
    }
}

/// A "pure value expression" over the comprehension / loop target: composed
/// only of the target itself, literals, provably-immutable captured names
/// (see [`RewriteCtx::captures`]), arithmetic / unary / comparison / boolean
/// operators, and pure calls to functions in `ctx.pure`. Everything it admits
/// is side-effect-free, so evaluating it under a parallel map (in any order)
/// preserves the observable result. Backs the filter conditions, the
/// reduction bodies, and each argument of a parallelisable element call.
pub(crate) fn is_pure_value_expr(expr: &Expr, target: &str, ctx: &RewriteCtx<'_>) -> bool {
    match expr {
        Expr::Name(n) => {
            let id = n.id.as_str();
            id == target || ctx.captures.contains(id)
        }
        Expr::NumberLiteral(_)
        | Expr::StringLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NoneLiteral(_) => true,
        Expr::BinOp(b) => {
            is_pure_value_expr(&b.left, target, ctx) && is_pure_value_expr(&b.right, target, ctx)
        }
        Expr::UnaryOp(u) => is_pure_value_expr(&u.operand, target, ctx),
        Expr::BoolOp(b) => b.values.iter().all(|e| is_pure_value_expr(e, target, ctx)),
        Expr::Compare(c) => {
            is_pure_value_expr(&c.left, target, ctx)
                && c.comparators
                    .iter()
                    .all(|e| is_pure_value_expr(e, target, ctx))
        }
        Expr::Call(c) => is_pure_call(c, target, ctx),
        _ => false,
    }
}

/// True when `call` is a call to a bare-name function in `ctx.pure` whose
/// every positional / keyword argument is itself a [`is_pure_value_expr`].
/// Rejects `*args` / `**kwargs` unpacking (the lambda wrapper can't
/// reconstruct those) and any non-pure callee.
fn is_pure_call(call: &ExprCall, target: &str, ctx: &RewriteCtx<'_>) -> bool {
    let Expr::Name(callee) = call.func.as_ref() else {
        return false;
    };
    if !ctx.pure.contains(callee.id.as_str()) {
        return false;
    }
    if call
        .arguments
        .args
        .iter()
        .any(|a| matches!(a, Expr::Starred(_)))
    {
        return false;
    }
    if call.arguments.keywords.iter().any(|k| k.arg.is_none()) {
        return false;
    }
    call.arguments
        .args
        .iter()
        .all(|a| is_pure_value_expr(a, target, ctx))
        && call
            .arguments
            .keywords
            .iter()
            .all(|k| is_pure_value_expr(&k.value, target, ctx))
}

/// True when `elt` is a parallelisable element: a pure call (see
/// [`is_pure_call`]) that mentions the loop target at least once.
fn elt_is_parallelisable(elt: &Expr, target: &str, ctx: &RewriteCtx<'_>) -> bool {
    matches!(elt, Expr::Call(c) if is_pure_call(c, target, ctx)) && mentions_name(elt, target)
}

/// Build the iterable a parallel map should run over. With no filter (`ifs`
/// empty) it's the original `iter`. With filters it's a sequential list
/// comprehension `[target for target in iter if COND ...]` — the (pure) filter
/// runs sequentially, the (pure) element map runs in parallel over the
/// survivors. Both are side-effect-free, so `[f(x) for x in xs if c]` and
/// `map_pure(lambda x: f(x), [x for x in xs if c])` are observationally
/// identical.
fn build_filtered_source(target: &str, iter: &Expr, ifs: &[Expr], range: TextRange) -> Expr {
    if ifs.is_empty() {
        return iter.clone();
    }
    let target_load = Expr::Name(ExprName {
        range,
        node_index: AtomicNodeIndex::NONE,
        id: Name::new(target),
        ctx: ExprContext::Load,
    });
    let target_store = Expr::Name(ExprName {
        range,
        node_index: AtomicNodeIndex::NONE,
        id: Name::new(target),
        ctx: ExprContext::Store,
    });
    Expr::ListComp(ExprListComp {
        range,
        node_index: AtomicNodeIndex::NONE,
        elt: Box::new(target_load),
        generators: vec![Comprehension {
            range,
            node_index: AtomicNodeIndex::NONE,
            target: target_store,
            iter: iter.clone(),
            ifs: ifs.to_vec(),
            is_async: false,
        }],
    })
}

// ── capturable-name analysis ──────────────────────────────────────────────────

/// Collect the set of names safe to capture by reference inside a parallel-map
/// lambda: names with positive `let`-binding evidence and no evidence of
/// mutation anywhere in the module.
///
/// A name qualifies when it is bound by an explicit `let` (`Mutability::Let`),
/// a module-level top-level assignment (implicit `let`), or a function
/// parameter — AND it is never bound `mut`, never an augmented-assignment
/// target, and never named in a `global` / `nonlocal` declaration. The scan is
/// module-wide (scope-insensitive): treating a name that is `mut` in ANY scope
/// as non-capturable everywhere only ever suppresses a rewrite, which is the
/// safe direction. Capturing a genuinely-immutable name is sound because
/// `map_pure` evaluates the whole map eagerly at the comprehension's position,
/// before control returns to the caller.
pub(crate) fn collect_capturable_names(module: &ModModule) -> HashSet<String> {
    let mut let_bound = HashSet::new();
    let mut mutated = HashSet::new();
    // Module-level bindings default to `let`.
    for stmt in &module.body {
        match stmt {
            Stmt::Assign(a) if a.mutability != Some(Mutability::Mut) => {
                for t in &a.targets {
                    if let Expr::Name(n) = t {
                        let_bound.insert(n.id.to_string());
                    }
                }
            }
            Stmt::AnnAssign(a) if a.mutability != Some(Mutability::Mut) => {
                if let Expr::Name(n) = a.target.as_ref() {
                    let_bound.insert(n.id.to_string());
                }
            }
            _ => {}
        }
    }
    for stmt in &module.body {
        scan_binding_evidence(stmt, &mut let_bound, &mut mutated);
    }
    let_bound.retain(|n| !mutated.contains(n));
    let_bound
}

/// Recursively record `let`-binding and mutation evidence for every name in
/// `stmt`. See [`collect_capturable_names`].
fn scan_binding_evidence(
    stmt: &Stmt,
    let_bound: &mut HashSet<String>,
    mutated: &mut HashSet<String>,
) {
    match stmt {
        Stmt::Assign(a) => {
            let store = match a.mutability {
                Some(Mutability::Mut) => &mut *mutated,
                Some(Mutability::Let) => &mut *let_bound,
                None => return, // an unclassified local assignment: no evidence
            };
            for t in &a.targets {
                if let Expr::Name(n) = t {
                    store.insert(n.id.to_string());
                }
            }
        }
        Stmt::AnnAssign(a) => {
            if let Expr::Name(n) = a.target.as_ref() {
                match a.mutability {
                    Some(Mutability::Mut) => {
                        mutated.insert(n.id.to_string());
                    }
                    Some(Mutability::Let) => {
                        let_bound.insert(n.id.to_string());
                    }
                    None => {}
                }
            }
        }
        Stmt::AugAssign(a) => {
            if let Expr::Name(n) = a.target.as_ref() {
                mutated.insert(n.id.to_string());
            }
        }
        Stmt::Global(g) => {
            for n in &g.names {
                mutated.insert(n.to_string());
            }
        }
        Stmt::Nonlocal(nl) => {
            for n in &nl.names {
                mutated.insert(n.to_string());
            }
        }
        Stmt::FunctionDef(f) => {
            for p in f
                .parameters
                .posonlyargs
                .iter()
                .chain(f.parameters.args.iter())
                .chain(f.parameters.kwonlyargs.iter())
            {
                let_bound.insert(p.parameter.name.to_string());
            }
            for extra in [
                f.parameters.vararg.as_deref(),
                f.parameters.kwarg.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                let_bound.insert(extra.name.to_string());
            }
            for s in &f.body {
                scan_binding_evidence(s, let_bound, mutated);
            }
        }
        Stmt::ClassDef(c) => {
            for s in &c.body {
                scan_binding_evidence(s, let_bound, mutated);
            }
        }
        Stmt::If(s) => {
            for s in &s.body {
                scan_binding_evidence(s, let_bound, mutated);
            }
            for clause in &s.elif_else_clauses {
                for s in &clause.body {
                    scan_binding_evidence(s, let_bound, mutated);
                }
            }
        }
        Stmt::While(s) => {
            for s in s.body.iter().chain(s.orelse.iter()) {
                scan_binding_evidence(s, let_bound, mutated);
            }
        }
        Stmt::For(s) => {
            // The loop target rebinds its name(s) every iteration — it is not a
            // stable invariant, so record it as mutated.
            collect_target_names(&s.target, mutated);
            for s in s.body.iter().chain(s.orelse.iter()) {
                scan_binding_evidence(s, let_bound, mutated);
            }
        }
        Stmt::With(s) => {
            // `with … as TARGET:` binds TARGET.
            for item in &s.items {
                if let Some(vars) = &item.optional_vars {
                    collect_target_names(vars, mutated);
                }
            }
            for s in &s.body {
                scan_binding_evidence(s, let_bound, mutated);
            }
        }
        Stmt::Try(s) => {
            for s in s
                .body
                .iter()
                .chain(s.orelse.iter())
                .chain(s.finalbody.iter())
            {
                scan_binding_evidence(s, let_bound, mutated);
            }
            for h in &s.handlers {
                let ExceptHandler::ExceptHandler(h) = h;
                // `except E as NAME:` binds (and later unbinds) NAME.
                if let Some(name) = &h.name {
                    mutated.insert(name.id.to_string());
                }
                for s in &h.body {
                    scan_binding_evidence(s, let_bound, mutated);
                }
            }
        }
        Stmt::Match(s) => {
            for case in &s.cases {
                // `case PATTERN:` captures rebind names (`case Point(x, y)`,
                // `case [*rest]`, `case {**rest}`, `case _ as n`, …).
                collect_pattern_names(&case.pattern, mutated);
                for s in &case.body {
                    scan_binding_evidence(s, let_bound, mutated);
                }
            }
        }
        _ => {}
    }
}

/// Insert every bare name bound by an assignment / `for` / `with` **target**
/// expression — `x`, `a, b`, `[a, b]`, `*rest`, and nested combinations — into
/// `out`. Attribute / subscript targets (`obj.x`, `xs[0]`) bind no bare name and
/// contribute nothing.
fn collect_target_names(target: &Expr, out: &mut HashSet<String>) {
    match target {
        Expr::Name(n) => {
            out.insert(n.id.to_string());
        }
        Expr::Tuple(t) => {
            for e in &t.elts {
                collect_target_names(e, out);
            }
        }
        Expr::List(l) => {
            for e in &l.elts {
                collect_target_names(e, out);
            }
        }
        Expr::Starred(s) => collect_target_names(&s.value, out),
        _ => {}
    }
}

/// Insert every name a `match` pattern captures into `out`: a bare capture
/// (`case NAME`), an `as` capture (`case … as NAME`), a sequence star
/// (`case [*rest]`), a mapping rest (`case {**rest}`), and every capture reached
/// through class / sequence / or sub-patterns. Mirrors the binding positions
/// `tyc-resolve`'s `walk_pattern` declares.
fn collect_pattern_names(pattern: &Pattern, out: &mut HashSet<String>) {
    match pattern {
        Pattern::MatchValue(_) | Pattern::MatchSingleton(_) => {}
        Pattern::MatchSequence(p) => {
            for sub in &p.patterns {
                collect_pattern_names(sub, out);
            }
        }
        Pattern::MatchMapping(p) => {
            for sub in &p.patterns {
                collect_pattern_names(sub, out);
            }
            if let Some(rest) = &p.rest {
                out.insert(rest.id.to_string());
            }
        }
        Pattern::MatchClass(p) => {
            for sub in &p.arguments.patterns {
                collect_pattern_names(sub, out);
            }
            for kw in &p.arguments.keywords {
                collect_pattern_names(&kw.pattern, out);
            }
        }
        Pattern::MatchStar(p) => {
            if let Some(name) = &p.name {
                out.insert(name.id.to_string());
            }
        }
        Pattern::MatchAs(p) => {
            if let Some(sub) = &p.pattern {
                collect_pattern_names(sub, out);
            }
            if let Some(name) = &p.name {
                out.insert(name.id.to_string());
            }
        }
        Pattern::MatchOr(p) => {
            for sub in &p.patterns {
                collect_pattern_names(sub, out);
            }
        }
    }
}

// ── detection (for the `tyc::parallel_opportunity` advice lint) ───────────────

/// Byte ranges of every comprehension in `module` that [`try_rewrite_listcomp`]
/// / [`try_rewrite_setcomp`] / [`try_rewrite_dictcomp`] would rewrite, without
/// mutating the module. Shares the exact eligibility predicates with the
/// rewrite, so the `tyc::parallel_opportunity` advice fires on precisely the
/// shapes `auto-parallel` would transform. `pure_callees` and `min_size` match
/// the rewrite's parameters.
pub fn detect_parallel_comprehensions(
    module: &ModModule,
    pure_callees: &HashSet<String>,
    min_size: u64,
) -> Vec<TextRange> {
    let captures = collect_capturable_names(module);
    let ctx = RewriteCtx {
        pure: pure_callees,
        min_size,
        captures: &captures,
    };
    let mut out = Vec::new();
    for stmt in &module.body {
        detect_in_stmt(stmt, &ctx, &mut out);
    }
    out
}

fn detect_in_stmt(stmt: &Stmt, ctx: &RewriteCtx<'_>, out: &mut Vec<TextRange>) {
    // Reuse the same structural traversal the rewriter walks, but collect
    // ranges instead of replacing nodes.
    match stmt {
        Stmt::FunctionDef(f) => {
            for s in &f.body {
                detect_in_stmt(s, ctx, out);
            }
        }
        Stmt::ClassDef(c) => {
            for s in &c.body {
                detect_in_stmt(s, ctx, out);
            }
        }
        Stmt::Assign(a) => detect_in_expr(&a.value, ctx, out),
        Stmt::AnnAssign(a) => {
            if let Some(v) = a.value.as_deref() {
                detect_in_expr(v, ctx, out);
            }
        }
        Stmt::AugAssign(a) => detect_in_expr(&a.value, ctx, out),
        Stmt::Expr(e) => detect_in_expr(&e.value, ctx, out),
        Stmt::Return(r) => {
            if let Some(v) = r.value.as_deref() {
                detect_in_expr(v, ctx, out);
            }
        }
        Stmt::If(i) => {
            detect_in_expr(&i.test, ctx, out);
            for s in &i.body {
                detect_in_stmt(s, ctx, out);
            }
            for clause in &i.elif_else_clauses {
                if let Some(test) = clause.test.as_ref() {
                    detect_in_expr(test, ctx, out);
                }
                for s in &clause.body {
                    detect_in_stmt(s, ctx, out);
                }
            }
        }
        Stmt::While(w) => {
            detect_in_expr(&w.test, ctx, out);
            for s in &w.body {
                detect_in_stmt(s, ctx, out);
            }
        }
        Stmt::For(f) => {
            detect_in_expr(&f.iter, ctx, out);
            for s in &f.body {
                detect_in_stmt(s, ctx, out);
            }
        }
        Stmt::With(w) => {
            for s in &w.body {
                detect_in_stmt(s, ctx, out);
            }
        }
        _ => {}
    }
}

fn detect_in_expr(expr: &Expr, ctx: &RewriteCtx<'_>, out: &mut Vec<TextRange>) {
    match expr {
        Expr::ListComp(lc) => {
            detect_in_expr(&lc.elt, ctx, out);
            for gen in &lc.generators {
                detect_in_expr(&gen.iter, ctx, out);
            }
            if analyse_comprehension(&lc.generators, &lc.elt, ctx, lc.range).is_some() {
                out.push(lc.range());
            }
        }
        Expr::SetComp(sc) => {
            detect_in_expr(&sc.elt, ctx, out);
            for gen in &sc.generators {
                detect_in_expr(&gen.iter, ctx, out);
            }
            if analyse_comprehension(&sc.generators, &sc.elt, ctx, sc.range).is_some() {
                out.push(sc.range());
            }
        }
        Expr::DictComp(dc) => {
            for gen in &dc.generators {
                detect_in_expr(&gen.iter, ctx, out);
            }
            if try_rewrite_dictcomp(dc, ctx).is_some() {
                out.push(dc.range());
            }
        }
        Expr::Call(c) => {
            detect_in_expr(&c.func, ctx, out);
            for arg in &c.arguments.args {
                detect_in_expr(arg, ctx, out);
            }
        }
        Expr::Tuple(t) => {
            for e in &t.elts {
                detect_in_expr(e, ctx, out);
            }
        }
        Expr::List(l) => {
            for e in &l.elts {
                detect_in_expr(e, ctx, out);
            }
        }
        _ => {}
    }
}

/// Statically-known length for a literal iterable expression.
///
/// Recognises `[a, b, c]` and `(a, b, c)` shapes and returns their element
/// count.  All other forms (`range(n)`, a bare name, a function call)
/// return `None`, signalling "unknown size" so the auto-parallel pass
/// proceeds as before.
pub(crate) fn literal_iter_len(expr: &Expr) -> Option<u64> {
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

    // ── Finding 4: binding positions that rebind a name must disqualify it ─────
    // from the capturable ("let-bound and never mutated") invariant set.

    #[test]
    fn let_bound_then_for_target_is_not_capturable() {
        let src = "\
let k: int = 1
for k in xs:
    print(k)
";
        let caps = collect_capturable_names(&parse(src));
        assert!(
            !caps.contains("k"),
            "a let binding rebound as a for-target is not an invariant: {caps:?}"
        );
    }

    #[test]
    fn let_bound_then_with_target_is_not_capturable() {
        let src = "\
let f: int = 1
with ctx() as f:
    print(f)
";
        let caps = collect_capturable_names(&parse(src));
        assert!(
            !caps.contains("f"),
            "a let binding rebound as a with-as target is not an invariant: {caps:?}"
        );
    }

    #[test]
    fn let_bound_then_except_alias_is_not_capturable() {
        let src = "\
let e: int = 1
try:
    print(e)
except ValueError as e:
    print(e)
";
        let caps = collect_capturable_names(&parse(src));
        assert!(
            !caps.contains("e"),
            "a let binding rebound as an except alias is not an invariant: {caps:?}"
        );
    }

    #[test]
    fn let_bound_then_match_capture_is_not_capturable() {
        // Covers a sequence capture, a mapping value capture, and an `as`
        // capture — every match-pattern binding position.
        let src = "\
let n: int = 1
let m: int = 2
let p: int = 3
match v:
    case [n]:
        print(n)
    case {\"k\": m}:
        print(m)
    case _ as p:
        print(p)
";
        let caps = collect_capturable_names(&parse(src));
        assert!(
            !caps.contains("n"),
            "match sequence capture must disqualify: {caps:?}"
        );
        assert!(
            !caps.contains("m"),
            "match mapping capture must disqualify: {caps:?}"
        );
        assert!(
            !caps.contains("p"),
            "match `as` capture must disqualify: {caps:?}"
        );
    }

    #[test]
    fn let_bound_invariant_never_rebound_stays_capturable() {
        // The fix must not over-reject: a let binding that is never rebound by
        // any construct remains a capturable invariant.
        let src = "\
let k: int = 1
ys = [f(x, k) for x in xs]
";
        let caps = collect_capturable_names(&parse(src));
        assert!(
            caps.contains("k"),
            "a never-rebound let binding must stay capturable: {caps:?}"
        );
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
    fn rewrites_filtered_comprehension_with_pure_filter() {
        // Widening (a): a pure `if` filter no longer vetoes — the rewrite
        // filters sequentially, maps in parallel.
        let src = "ys = [f(x) for x in xs if x > 0]\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(
            stats.rewrites, 1,
            "pure filter should be handled, not vetoed"
        );
        let emit = tyc_emit::emit_python(&m);
        assert!(
            emit.contains("typhon_runtime.parallel.map_pure"),
            "expected map_pure; got: {emit}"
        );
        assert!(
            emit.contains("for x in xs if x > 0"),
            "expected filtered source comprehension; got: {emit}"
        );
    }

    #[test]
    fn leaves_impure_filtered_comprehension_alone() {
        // The filter calls an impure function `g` — must NOT rewrite.
        let src = "ys = [f(x) for x in xs if g(x)]\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(stats.rewrites, 0, "impure filter must veto the rewrite");
    }

    #[test]
    fn leaves_nested_comprehension_alone() {
        let src = "ys = [f(x) for row in rows for x in row]\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(stats.rewrites, 0);
    }

    #[test]
    fn rewrites_multi_arg_call_with_literal() {
        // Widening (b): a literal extra argument is safe to capture.
        let src = "ys = [f(x, 1) for x in xs]\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(stats.rewrites, 1, "literal extra arg should be captured");
        let emit = tyc_emit::emit_python(&m);
        assert!(emit.contains("lambda x: f(x, 1)"), "got: {emit}");
    }

    #[test]
    fn rewrites_multi_arg_call_with_let_invariant() {
        // Widening (b): a `let`-bound loop invariant is capturable.
        let src = "\
def run(xs: list[int]) -> list[int]:
    let k: int = 3
    return [f(x, k) for x in xs]
";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(stats.rewrites, 1, "let-bound invariant should be captured");
        let emit = tyc_emit::emit_python(&m);
        assert!(emit.contains("map_pure(lambda x: f(x, k)"), "got: {emit}");
    }

    #[test]
    fn leaves_multi_arg_call_with_mut_invariant_alone() {
        // Widening (b) guard: a `mut`-bound extra arg is NOT a safe capture —
        // the rewrite must be suppressed.
        let src = "\
def run(xs: list[int]) -> list[int]:
    mut k: int = 3
    k = 4
    return [f(x, k) for x in xs]
";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(
            stats.rewrites, 0,
            "mut-bound extra arg must veto the rewrite"
        );
    }

    #[test]
    fn rewrites_nested_pure_call() {
        // Widening (c): `g(f(x))` with both pure.
        let src = "ys = [g(f(x)) for x in xs]\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f", "g"]), 0);
        assert_eq!(stats.rewrites, 1, "nested pure calls should rewrite");
        let emit = tyc_emit::emit_python(&m);
        assert!(emit.contains("lambda x: g(f(x))"), "got: {emit}");
    }

    #[test]
    fn leaves_nested_call_with_impure_inner_alone() {
        // `g` is pure but the inner `f` is not — must NOT rewrite.
        let src = "ys = [g(f(x)) for x in xs]\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["g"]), 0);
        assert_eq!(stats.rewrites, 0, "impure inner call must veto");
    }

    #[test]
    fn leaves_element_without_target_alone() {
        // `f(k)` ignores the loop target — parallelising it is pointless and
        // suppressed.
        let src = "\
def run(xs: list[int]) -> list[int]:
    let k: int = 3
    return [f(k) for x in xs]
";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(stats.rewrites, 0, "element must mention the loop target");
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
    fn rewrites_filtered_setcomp_with_pure_filter() {
        let src = "ys = {f(x) for x in xs if x > 0}\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(
            stats.rewrites, 1,
            "pure filter on a set comp should rewrite"
        );
        let emit = tyc_emit::emit_python(&m);
        assert!(emit.contains("{*typhon_runtime"), "got: {emit}");
        assert!(emit.contains("for x in xs if x > 0"), "got: {emit}");
    }

    #[test]
    fn leaves_impure_setcomp_alone() {
        let src = "ys = {g(x) for x in xs}\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(stats.rewrites, 0);
    }

    // ── dict comprehensions ────────────────────────────────────────────────

    #[test]
    fn rewrites_pure_dictcomp_value_pure() {
        // Pattern: {simple_key: f(x) for x in xs}
        let src = "ys = {x: f(x) for x in xs}\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(stats.rewrites, 1, "dict comprehension should rewrite");
        let emit = tyc_emit::emit_python(&m);
        assert!(
            emit.contains("typhon_runtime.parallel.map_pure"),
            "expected map_pure rewrite; got: {emit}"
        );
        // Should emit `{k: v for (k, v) in map_pure(...)}` not `dict(...)`
        assert!(
            !emit.contains("dict("),
            "should not depend on the `dict` name; got: {emit}"
        );
        assert!(
            emit.contains("for (_k, _v)") || emit.contains("for (_k,_v)"),
            "expected tuple unpacking in comprehension; got: {emit}"
        );
    }

    #[test]
    fn rewrites_pure_dictcomp_key_pure() {
        // Pattern: {f(x): simple_value for x in xs}
        let src = "ys = {f(x): x for x in xs}\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(stats.rewrites, 1, "dict comprehension should rewrite");
        let emit = tyc_emit::emit_python(&m);
        assert!(
            emit.contains("typhon_runtime.parallel.map_pure"),
            "expected map_pure rewrite; got: {emit}"
        );
        assert!(
            !emit.contains("dict("),
            "should not depend on the `dict` name; got: {emit}"
        );
    }

    #[test]
    fn rewrites_pure_dictcomp_literal_key() {
        // Pattern: {literal: f(x) for x in xs}
        let src = "ys = {'key': f(x) for x in xs}\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(
            stats.rewrites, 1,
            "dict comprehension with literal key should rewrite"
        );
    }

    #[test]
    fn leaves_both_pure_dictcomp_alone() {
        // Neither key nor value is simple — cannot rewrite
        let src = "ys = {f(x): g(x) for x in xs}\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f", "g"]), 0);
        assert_eq!(
            stats.rewrites, 0,
            "both key and value are pure calls — ineligible"
        );
    }

    #[test]
    fn leaves_both_simple_dictcomp_alone() {
        // Both key and value are simple — no pure call
        let src = "ys = {x: x for x in xs}\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(stats.rewrites, 0, "no pure call present");
    }

    #[test]
    fn rewrites_filtered_dictcomp_with_pure_filter() {
        let src = "ys = {x: f(x) for x in xs if x > 0}\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(
            stats.rewrites, 1,
            "pure filter on a dict comp should rewrite"
        );
        let emit = tyc_emit::emit_python(&m);
        assert!(
            emit.contains("typhon_runtime.parallel.map_pure"),
            "got: {emit}"
        );
        assert!(emit.contains("for x in xs if x > 0"), "got: {emit}");
    }

    #[test]
    fn leaves_nested_dictcomp_alone() {
        let src = "ys = {x: f(x) for row in rows for x in row}\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(stats.rewrites, 0, "nested generators must veto the rewrite");
    }

    #[test]
    fn leaves_impure_dictcomp_alone() {
        let src = "ys = {x: g(x) for x in xs}\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 0);
        assert_eq!(stats.rewrites, 0, "impure callee must veto the rewrite");
    }

    #[test]
    fn dictcomp_min_size_threshold_suppresses_short_literal_iters() {
        let src = "ys = {x: f(x) for x in [1, 2, 3]}\n";
        let mut m = parse(src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 64);
        assert_eq!(
            stats.rewrites, 0,
            "literal iter shorter than threshold must not rewrite"
        );
    }

    #[test]
    fn dictcomp_min_size_threshold_passes_long_literal_iters() {
        let elts = (0..64)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let src = format!("ys = {{x: f(x) for x in [{elts}]}}\n");
        let mut m = parse(&src);
        let stats = rewrite_parallel_comprehensions(&mut m, &pure_set(&["f"]), 64);
        assert_eq!(stats.rewrites, 1);
    }

    // ── detection (parallel_opportunity lint) ──────────────────────────────

    #[test]
    fn detect_matches_rewrite_eligibility() {
        // The detector must flag exactly the eligible comprehensions and no
        // more — here a pure list comp is flagged, the impure one is not.
        let src = "\
a = [f(x) for x in xs]
b = [g(x) for x in xs]
";
        let m = parse(src);
        let hits = detect_parallel_comprehensions(&m, &pure_set(&["f"]), 0);
        assert_eq!(
            hits.len(),
            1,
            "only the pure comprehension should be flagged"
        );
    }

    #[test]
    fn detect_flags_filtered_and_nested_shapes() {
        let src = "\
def run(xs: list[int]) -> None:
    let k: int = 2
    a = [f(x) for x in xs if x > 0]
    b = [g(f(x)) for x in xs]
    c = [f(x, k) for x in xs]
";
        let m = parse(src);
        let hits = detect_parallel_comprehensions(&m, &pure_set(&["f", "g"]), 0);
        assert_eq!(hits.len(), 3, "all three widened shapes should be detected");
    }
}
