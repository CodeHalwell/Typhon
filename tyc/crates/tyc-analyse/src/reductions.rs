//! Integer accumulator-loop reduction (`[strictness] auto-parallel-reductions`).
//!
//! Rewrites the canonical accumulation loop
//!
//! ```text
//! for x in ITER:
//!     total += EXPR
//! ```
//!
//! into a parallel map-then-`sum`:
//!
//! ```text
//! total += sum(typhon_runtime.parallel.map_pure(lambda x: EXPR, ITER))
//! ```
//!
//! The rewrite is only sound — and therefore only fires — when the
//! accumulator is a **plain `int`** (`mut total: int`). Integer addition is
//! associative and commutative and Python's ints are exact, so summing the
//! per-element partial results in any order yields the identical value.
//! **Floats are never eligible**: reordering float addition changes the
//! result (`(a + b) + c != a + (b + c)` under IEEE-754 rounding), so a
//! `mut total: float` loop is left untouched (and instead surfaced by the
//! `tyc::parallel_opportunity` advice as a shape that would need a manual
//! refactor).
//!
//! Every eligibility condition:
//!
//!   1. **Loop shape.** `for TARGET in ITER:` with a bare-name `TARGET`, no
//!      `else` clause, and a body of *exactly one* statement.
//!   2. **Accumulation.** That statement is `ACC += EXPR` or the equivalent
//!      `ACC = ACC + EXPR`, where `ACC` is a plain name.
//!   3. **Int accumulator.** `ACC` is declared `mut ACC: int` somewhere in the
//!      module (the `int` annotation is required).
//!   4. **Pure element.** `EXPR` is a [`crate::parallel::is_pure_value_expr`]
//!      over the loop target — the target, literals, `let`-bound loop
//!      invariants, arithmetic / comparison / boolean operators, and calls to
//!      pure functions — and it mentions the target at least once.
//!   5. **Invariant iterable.** `ITER` does not reference the accumulator.
//!
//! Gated at the call site on `auto-parallel` **and** `auto-parallel-reductions`
//! both being on. Honours `[strictness] parallel-min-size` for statically-sized
//! literal iterables exactly like the comprehension rewrite.

use std::collections::HashSet;

use ruff_python_ast::{
    name::Name, Arguments, AtomicNodeIndex, Expr, ExprCall, ExprName, ModModule, Mutability,
    Operator, Stmt, StmtAugAssign,
};
use ruff_text_size::{Ranged, TextRange};

use crate::parallel::{
    build_map_pure_call, collect_capturable_names, is_pure_value_expr, literal_iter_len,
    mentions_name, RewriteCtx,
};

/// Summary of what the reduction pass rewrote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReductionStats {
    /// Number of accumulator loops converted to `sum(map_pure(...))`.
    pub rewrites: usize,
}

/// One matched accumulator loop: the accumulator name, the loop target, the
/// pure element expression, and the (invariant) iterable.
struct ReductionMatch {
    acc: String,
    target: String,
    expr: Expr,
    iter: Expr,
    /// Byte range of the original `for` statement (for diagnostics).
    range: TextRange,
}

/// Rewrite eligible integer accumulator loops into `total += sum(map_pure(...))`.
///
/// `pure_callees` and `min_size` mirror the comprehension rewrite's parameters
/// (and share the pure-name set at the build call site).
pub fn rewrite_reduction_loops(
    module: &mut ModModule,
    pure_callees: &HashSet<String>,
    min_size: u64,
) -> ReductionStats {
    let captures = collect_capturable_names(module);
    // Typed-`mut` accumulators are resolved **per scope**, not module-wide: the
    // set here covers only the module scope, and the traversal recomputes a
    // fresh set on entry to each `def` / `class` body. This is what stops a
    // `mut total: int` in one function from making a same-named
    // `mut total: float` accumulator in another look int-typed — which would let
    // the rewrite reorder that function's float addition and change results, the
    // one thing the reduction rewrite must never do.
    let scope_mut = collect_scope_typed_mut(&module.body, "int");
    // The rewrite emits a call to the bare name `sum`; if user code rebinds
    // `sum` anywhere visible to the loop (module scope here; each function
    // scope below), the emitted call would resolve to the user binding instead
    // of `builtins.sum` — a behaviour change. Track shadowing per scope and
    // suppress the rewrite conservatively.
    let sum_shadowed = scope_binds_name(&module.body, "sum");
    let ctx = RewriteCtx::new(pure_callees, min_size, &captures);
    let mut stats = ReductionStats::default();
    rewrite_stmts(&mut module.body, &ctx, &scope_mut, sum_shadowed, &mut stats);
    stats
}

fn rewrite_stmts(
    body: &mut [Stmt],
    ctx: &RewriteCtx<'_>,
    scope_mut: &HashSet<String>,
    sum_shadowed: bool,
    stats: &mut ReductionStats,
) {
    for stmt in body.iter_mut() {
        // Recurse first so nested loops inside a matched loop's *sibling*
        // blocks are considered (a matched leaf loop has no nested statements
        // to recurse into anyway).
        recurse_children(stmt, ctx, scope_mut, sum_shadowed, stats);
        if let Stmt::For(f) = stmt {
            if let Some(m) = match_reduction_loop(f, ctx, scope_mut) {
                if !sum_shadowed && !under_min_size(&m.iter, ctx.min_size) {
                    *stmt = build_reduction_stmt(&m);
                    stats.rewrites += 1;
                }
            }
        }
    }
}

fn recurse_children(
    stmt: &mut Stmt,
    ctx: &RewriteCtx<'_>,
    scope_mut: &HashSet<String>,
    sum_shadowed: bool,
    stats: &mut ReductionStats,
) {
    match stmt {
        // A `def` / `class` opens a new scope: recompute the typed-`mut` set
        // from that body so accumulators from the enclosing scope don't leak in
        // (and this scope's don't leak out). `sum`-shadowing accumulates the
        // other way: a binding in any *enclosing* scope stays visible inside,
        // so OR the outer flag with this scope's own evidence (parameters
        // included — a parameter named `sum` shadows the builtin too).
        Stmt::FunctionDef(f) => {
            let inner = collect_scope_typed_mut(&f.body, "int");
            let shadowed = sum_shadowed
                || params_bind_name(&f.parameters, "sum")
                || scope_binds_name(&f.body, "sum");
            rewrite_stmts(&mut f.body, ctx, &inner, shadowed, stats);
        }
        Stmt::ClassDef(c) => {
            let inner = collect_scope_typed_mut(&c.body, "int");
            let shadowed = sum_shadowed || scope_binds_name(&c.body, "sum");
            rewrite_stmts(&mut c.body, ctx, &inner, shadowed, stats);
        }
        // Control-flow blocks share the enclosing scope: thread it unchanged.
        Stmt::If(s) => {
            rewrite_stmts(&mut s.body, ctx, scope_mut, sum_shadowed, stats);
            for clause in &mut s.elif_else_clauses {
                rewrite_stmts(&mut clause.body, ctx, scope_mut, sum_shadowed, stats);
            }
        }
        Stmt::While(s) => {
            rewrite_stmts(&mut s.body, ctx, scope_mut, sum_shadowed, stats);
            rewrite_stmts(&mut s.orelse, ctx, scope_mut, sum_shadowed, stats);
        }
        Stmt::For(s) => {
            rewrite_stmts(&mut s.body, ctx, scope_mut, sum_shadowed, stats);
            rewrite_stmts(&mut s.orelse, ctx, scope_mut, sum_shadowed, stats);
        }
        Stmt::With(s) => rewrite_stmts(&mut s.body, ctx, scope_mut, sum_shadowed, stats),
        Stmt::Try(s) => {
            rewrite_stmts(&mut s.body, ctx, scope_mut, sum_shadowed, stats);
            rewrite_stmts(&mut s.orelse, ctx, scope_mut, sum_shadowed, stats);
            rewrite_stmts(&mut s.finalbody, ctx, scope_mut, sum_shadowed, stats);
            for h in &mut s.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                rewrite_stmts(&mut h.body, ctx, scope_mut, sum_shadowed, stats);
            }
        }
        Stmt::Match(s) => {
            for case in &mut s.cases {
                rewrite_stmts(&mut case.body, ctx, scope_mut, sum_shadowed, stats);
            }
        }
        _ => {}
    }
}

/// Match a `for` loop against the reduction shape, checking eligibility
/// against `int_mut` (the set of `mut NAME: int` names) and `ctx`.
fn match_reduction_loop(
    f: &ruff_python_ast::StmtFor,
    ctx: &RewriteCtx<'_>,
    typed_mut: &HashSet<String>,
) -> Option<ReductionMatch> {
    if f.is_async || !f.orelse.is_empty() || f.body.len() != 1 {
        return None;
    }
    let target = match f.target.as_ref() {
        Expr::Name(n) => n.id.as_str().to_owned(),
        _ => return None,
    };
    let (acc, expr) = match_accumulation(&f.body[0])?;
    // The accumulator must be a typed-`mut` binding of the right kind.
    if !typed_mut.contains(&acc) {
        return None;
    }
    // The element must be a pure expression over the target that actually
    // mentions the target (and hence can't be the accumulator, which is `mut`
    // and therefore not a capture).
    if !is_pure_value_expr(expr, &target, ctx) || !mentions_name(expr, &target) {
        return None;
    }
    // The iterable must be loop-invariant with respect to the accumulator.
    if mentions_name_anywhere(&f.iter, &acc) {
        return None;
    }
    Some(ReductionMatch {
        acc,
        target,
        expr: expr.clone(),
        iter: f.iter.as_ref().clone(),
        range: f.range(),
    })
}

/// Recognise `ACC += EXPR` or `ACC = ACC + EXPR`. Returns `(acc, expr)`.
fn match_accumulation(stmt: &Stmt) -> Option<(String, &Expr)> {
    match stmt {
        Stmt::AugAssign(a) if matches!(a.op, Operator::Add) => {
            let acc = match a.target.as_ref() {
                Expr::Name(n) => n.id.as_str().to_owned(),
                _ => return None,
            };
            Some((acc, a.value.as_ref()))
        }
        Stmt::Assign(a) if a.targets.len() == 1 => {
            let acc = match &a.targets[0] {
                Expr::Name(n) => n.id.as_str().to_owned(),
                _ => return None,
            };
            // Right side must be `ACC + EXPR`.
            let Expr::BinOp(b) = a.value.as_ref() else {
                return None;
            };
            if !matches!(b.op, Operator::Add) {
                return None;
            }
            match b.left.as_ref() {
                Expr::Name(n) if n.id.as_str() == acc => Some((acc, b.right.as_ref())),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Build `ACC += sum(typhon_runtime.parallel.map_pure(lambda TARGET: EXPR, ITER))`.
fn build_reduction_stmt(m: &ReductionMatch) -> Stmt {
    let range = m.range;
    let map_call = build_map_pure_call(range, m.target.clone(), m.iter.clone(), m.expr.clone());
    let sum_call = Expr::Call(ExprCall {
        range,
        node_index: AtomicNodeIndex::NONE,
        func: Box::new(Expr::Name(ExprName {
            range,
            node_index: AtomicNodeIndex::NONE,
            id: Name::new("sum"),
            ctx: ruff_python_ast::ExprContext::Load,
        })),
        arguments: Arguments {
            range,
            node_index: AtomicNodeIndex::NONE,
            args: vec![map_call].into_boxed_slice(),
            keywords: Vec::new().into_boxed_slice(),
        },
    });
    Stmt::AugAssign(StmtAugAssign {
        range,
        node_index: AtomicNodeIndex::NONE,
        target: Box::new(Expr::Name(ExprName {
            range,
            node_index: AtomicNodeIndex::NONE,
            id: Name::new(&m.acc),
            ctx: ruff_python_ast::ExprContext::Store,
        })),
        op: Operator::Add,
        value: Box::new(sum_call),
    })
}

/// True when a statically-sized literal iterable is shorter than `min_size`.
fn under_min_size(iter: &Expr, min_size: u64) -> bool {
    literal_iter_len(iter).is_some_and(|n| n < min_size)
}

/// Collect every name declared `mut NAME: <ann>` **within a single
/// function/module scope** — the given `body` plus the nested control-flow
/// blocks that share that scope (`if` / `while` / `for` / `with` / `try` /
/// `match`), but **not** nested `def` / `class` bodies, which open their own
/// scope. `<ann>` is the bare type name (`"int"` / `"float"`).
///
/// Scoping this (rather than flattening every `mut NAME: <ann>` in the module
/// into one set) is the fix for the cross-function contamination the reduction
/// rewrite must avoid: an `int` accumulator in function A and a same-named
/// `float` accumulator in function B are different bindings, and B's loop must
/// stay sequential — reordering IEEE-754 addition changes the result. The
/// caller recomputes a fresh set on entry to each `def` / `class`.
fn collect_scope_typed_mut(body: &[Stmt], ann: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    collect_scope_typed_mut_into(body, ann, &mut out);
    out
}

fn collect_scope_typed_mut_into(body: &[Stmt], ann: &str, out: &mut HashSet<String>) {
    for stmt in body {
        match stmt {
            Stmt::AnnAssign(a) if a.mutability == Some(Mutability::Mut) => {
                if let (Expr::Name(target), Expr::Name(ty)) =
                    (a.target.as_ref(), a.annotation.as_ref())
                {
                    if ty.id.as_str() == ann {
                        out.insert(target.id.to_string());
                    }
                }
            }
            // Control-flow blocks share the enclosing scope — descend.
            Stmt::If(s) => {
                collect_scope_typed_mut_into(&s.body, ann, out);
                for clause in &s.elif_else_clauses {
                    collect_scope_typed_mut_into(&clause.body, ann, out);
                }
            }
            Stmt::While(s) => {
                collect_scope_typed_mut_into(&s.body, ann, out);
                collect_scope_typed_mut_into(&s.orelse, ann, out);
            }
            Stmt::For(s) => {
                collect_scope_typed_mut_into(&s.body, ann, out);
                collect_scope_typed_mut_into(&s.orelse, ann, out);
            }
            Stmt::With(s) => collect_scope_typed_mut_into(&s.body, ann, out),
            Stmt::Try(s) => {
                collect_scope_typed_mut_into(&s.body, ann, out);
                collect_scope_typed_mut_into(&s.orelse, ann, out);
                collect_scope_typed_mut_into(&s.finalbody, ann, out);
                for h in &s.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    collect_scope_typed_mut_into(&h.body, ann, out);
                }
            }
            Stmt::Match(s) => {
                for case in &s.cases {
                    collect_scope_typed_mut_into(&case.body, ann, out);
                }
            }
            // `def` / `class` open their own scope — do NOT descend. The
            // traversal recomputes a fresh set when it enters them.
            _ => {}
        }
    }
}

/// True when `name` appears anywhere inside `expr`. Unlike
/// [`crate::parallel::mentions_name`], this walks the *general* expression
/// tree (used to check iterable invariance against the accumulator, where the
/// iterable can be an arbitrary expression like `range(len(rows))`).
fn mentions_name_anywhere(expr: &Expr, name: &str) -> bool {
    use ruff_python_ast::visitor::source_order::{walk_expr, SourceOrderVisitor};
    struct V<'a> {
        name: &'a str,
        found: bool,
    }
    impl<'ast, 'a> SourceOrderVisitor<'ast> for V<'a> {
        fn visit_expr(&mut self, e: &'ast Expr) {
            if let Expr::Name(n) = e {
                if n.id.as_str() == self.name {
                    self.found = true;
                }
            }
            if !self.found {
                walk_expr(self, e);
            }
        }
    }
    let mut v = V { name, found: false };
    v.visit_expr(expr);
    v.found
}

/// True when any parameter of `parameters` (positional-only, positional,
/// keyword-only, `*args`, `**kwargs`) is named `name`.
fn params_bind_name(parameters: &ruff_python_ast::Parameters, name: &str) -> bool {
    parameters
        .posonlyargs
        .iter()
        .chain(parameters.args.iter())
        .chain(parameters.kwonlyargs.iter())
        .any(|p| p.parameter.name.as_str() == name)
        || [parameters.vararg.as_deref(), parameters.kwarg.as_deref()]
            .into_iter()
            .flatten()
            .any(|p| p.name.as_str() == name)
}

/// True when `name` is (or may be) **bound** within a single scope body — the
/// given statements plus the control-flow blocks that share the scope, but not
/// nested `def` / `class` bodies (which open their own scope; only their
/// *names* bind here, though their decorators / parameter defaults / class
/// arguments still evaluate — and can walrus-bind — in this scope).
///
/// Binding evidence: any `Name` in Store / Del context (assignments, `for` /
/// `with` targets, walrus, tuple unpacking, `del`), `def NAME` / `class NAME`,
/// import aliases (a `from m import *` conservatively counts as binding
/// anything), `global NAME` / `nonlocal NAME` declarations, `except … as
/// NAME`, and `match` pattern captures. Used to keep the reduction rewrite
/// from emitting a bare `sum` call where user code rebinds `sum`; false
/// positives merely skip an optimisation, so ambiguity resolves to `true`.
fn scope_binds_name(body: &[Stmt], name: &str) -> bool {
    use ruff_python_ast::visitor::source_order::{
        walk_expr, walk_pattern, walk_stmt, SourceOrderVisitor,
    };
    use ruff_python_ast::Pattern;

    struct V<'a> {
        name: &'a str,
        found: bool,
    }
    impl<'ast, 'a> SourceOrderVisitor<'ast> for V<'a> {
        fn visit_stmt(&mut self, s: &'ast Stmt) {
            if self.found {
                return;
            }
            match s {
                // A nested `def` binds its *name* in this scope; its body is a
                // different scope. Decorators and parameter defaults evaluate
                // here, so walk those for walrus bindings.
                Stmt::FunctionDef(f) => {
                    if f.name.as_str() == self.name {
                        self.found = true;
                        return;
                    }
                    for dec in &f.decorator_list {
                        self.visit_expr(&dec.expression);
                    }
                    for p in f
                        .parameters
                        .posonlyargs
                        .iter()
                        .chain(f.parameters.args.iter())
                        .chain(f.parameters.kwonlyargs.iter())
                    {
                        if let Some(default) = p.default.as_deref() {
                            self.visit_expr(default);
                        }
                    }
                }
                // Same for a nested `class`: name binds here, body doesn't;
                // decorators and base/keyword arguments evaluate here.
                Stmt::ClassDef(c) => {
                    if c.name.as_str() == self.name {
                        self.found = true;
                        return;
                    }
                    for dec in &c.decorator_list {
                        self.visit_expr(&dec.expression);
                    }
                    if let Some(args) = c.arguments.as_deref() {
                        for arg in &args.args {
                            self.visit_expr(arg);
                        }
                        for kw in &args.keywords {
                            self.visit_expr(&kw.value);
                        }
                    }
                }
                Stmt::Import(i) => {
                    for alias in &i.names {
                        let bound = match &alias.asname {
                            Some(asname) => asname.as_str(),
                            // `import a.b` binds `a`.
                            None => alias.name.as_str().split('.').next().unwrap_or(""),
                        };
                        if bound == self.name {
                            self.found = true;
                            return;
                        }
                    }
                }
                Stmt::ImportFrom(i) => {
                    for alias in &i.names {
                        let bound = match &alias.asname {
                            Some(asname) => asname.as_str(),
                            None => alias.name.as_str(),
                        };
                        // `from m import *` may bind anything — conservative.
                        if bound == self.name || bound == "*" {
                            self.found = true;
                            return;
                        }
                    }
                }
                // A `global` / `nonlocal` declaration makes assignments in
                // this scope rebind the outer name — conservative evidence.
                Stmt::Global(g) => {
                    if g.names.iter().any(|n| n.as_str() == self.name) {
                        self.found = true;
                    }
                }
                Stmt::Nonlocal(nl) => {
                    if nl.names.iter().any(|n| n.as_str() == self.name) {
                        self.found = true;
                    }
                }
                // `except E as NAME:` — the alias is an Identifier, not an
                // `Expr::Name`, so check it here; the bodies walk normally.
                Stmt::Try(t) => {
                    for h in &t.handlers {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                        if let Some(alias) = &h.name {
                            if alias.id.as_str() == self.name {
                                self.found = true;
                                return;
                            }
                        }
                    }
                    walk_stmt(self, s);
                }
                _ => walk_stmt(self, s),
            }
        }

        fn visit_expr(&mut self, e: &'ast Expr) {
            if self.found {
                return;
            }
            // Store covers assignment / loop / `with` targets, tuple unpacks,
            // and walrus targets; Del covers `del NAME` (rebinding evidence
            // either way).
            if let Expr::Name(n) = e {
                if n.id.as_str() == self.name && !n.ctx.is_load() {
                    self.found = true;
                    return;
                }
            }
            walk_expr(self, e);
        }

        fn visit_pattern(&mut self, p: &'ast Pattern) {
            if self.found {
                return;
            }
            let captured = match p {
                Pattern::MatchAs(m) => m.name.as_ref(),
                Pattern::MatchStar(m) => m.name.as_ref(),
                Pattern::MatchMapping(m) => m.rest.as_ref(),
                _ => None,
            };
            if captured.is_some_and(|id| id.as_str() == self.name) {
                self.found = true;
                return;
            }
            walk_pattern(self, p);
        }
    }

    let mut v = V { name, found: false };
    for stmt in body {
        v.visit_stmt(stmt);
        if v.found {
            return true;
        }
    }
    false
}

// ── detection (for the `tyc::parallel_opportunity` advice lint) ───────────────

/// A reduction shape the `tyc::parallel_opportunity` advice reports.
#[derive(Debug, Clone)]
pub struct ReductionHit {
    /// The accumulator name (for the message).
    pub acc: String,
    /// Byte range of the `for` loop.
    pub range: TextRange,
    /// `true` when the accumulator is a `float` (the "would be parallelisable
    /// except addition reordering changes float results" case), `false` for an
    /// eligible `int` accumulator.
    pub is_float: bool,
}

/// Detect every accumulator loop that [`rewrite_reduction_loops`] would
/// transform (`int` accumulator), plus the `float`-accumulator loops that
/// match every condition except the int annotation. The lint uses the `int`
/// hits when `auto-parallel-reductions` is off, and the `float` hits always
/// (a float reduction is never auto-parallelised).
pub fn detect_reduction_loops(
    module: &ModModule,
    pure_callees: &HashSet<String>,
    min_size: u64,
) -> Vec<ReductionHit> {
    let captures = collect_capturable_names(module);
    // Per-scope, like the rewrite (see `rewrite_reduction_loops`): recomputed on
    // entry to each `def` / `class` so the advice keys on the accumulator's type
    // in its own scope, never a same-named binding from another function.
    let int_mut = collect_scope_typed_mut(&module.body, "int");
    let float_mut = collect_scope_typed_mut(&module.body, "float");
    // Mirror the rewrite's bare-`sum` shadowing suppression so the int hits
    // report precisely the loops `rewrite_reduction_loops` would transform.
    let sum_shadowed = scope_binds_name(&module.body, "sum");
    let ctx = RewriteCtx::new(pure_callees, min_size, &captures);
    let mut out = Vec::new();
    detect_in_stmts(
        &module.body,
        &ctx,
        &int_mut,
        &float_mut,
        sum_shadowed,
        &mut out,
    );
    out
}

fn detect_in_stmts(
    body: &[Stmt],
    ctx: &RewriteCtx<'_>,
    int_mut: &HashSet<String>,
    float_mut: &HashSet<String>,
    sum_shadowed: bool,
    out: &mut Vec<ReductionHit>,
) {
    for stmt in body {
        detect_children(stmt, ctx, int_mut, float_mut, sum_shadowed, out);
        if let Stmt::For(f) = stmt {
            // int accumulator → eligible for the rewrite (unless a user
            // binding of `sum` would capture the rewrite's emitted call).
            if !sum_shadowed {
                if let Some(m) = match_reduction_loop(f, ctx, int_mut) {
                    if !under_min_size(&m.iter, ctx.min_size) {
                        out.push(ReductionHit {
                            acc: m.acc,
                            range: m.range,
                            is_float: false,
                        });
                        continue;
                    }
                }
            }
            // float accumulator → matches everything but the int annotation.
            // (Independent of `sum` shadowing: this advice is about the float
            // reordering barrier, not the emitted `sum(...)` call.)
            if let Some(m) = match_reduction_loop(f, ctx, float_mut) {
                if !under_min_size(&m.iter, ctx.min_size) {
                    out.push(ReductionHit {
                        acc: m.acc,
                        range: m.range,
                        is_float: true,
                    });
                }
            }
        }
    }
}

fn detect_children(
    stmt: &Stmt,
    ctx: &RewriteCtx<'_>,
    int_mut: &HashSet<String>,
    float_mut: &HashSet<String>,
    sum_shadowed: bool,
    out: &mut Vec<ReductionHit>,
) {
    match stmt {
        // New scope: recompute both sets from that body (see `detect_children`'s
        // sibling in the rewrite path, `recurse_children`); `sum` shadowing ORs
        // downward exactly like the rewrite path.
        Stmt::FunctionDef(f) => {
            let int_inner = collect_scope_typed_mut(&f.body, "int");
            let float_inner = collect_scope_typed_mut(&f.body, "float");
            let shadowed = sum_shadowed
                || params_bind_name(&f.parameters, "sum")
                || scope_binds_name(&f.body, "sum");
            detect_in_stmts(&f.body, ctx, &int_inner, &float_inner, shadowed, out);
        }
        Stmt::ClassDef(c) => {
            let int_inner = collect_scope_typed_mut(&c.body, "int");
            let float_inner = collect_scope_typed_mut(&c.body, "float");
            let shadowed = sum_shadowed || scope_binds_name(&c.body, "sum");
            detect_in_stmts(&c.body, ctx, &int_inner, &float_inner, shadowed, out);
        }
        Stmt::If(s) => {
            detect_in_stmts(&s.body, ctx, int_mut, float_mut, sum_shadowed, out);
            for clause in &s.elif_else_clauses {
                detect_in_stmts(&clause.body, ctx, int_mut, float_mut, sum_shadowed, out);
            }
        }
        Stmt::While(s) => {
            detect_in_stmts(&s.body, ctx, int_mut, float_mut, sum_shadowed, out);
            detect_in_stmts(&s.orelse, ctx, int_mut, float_mut, sum_shadowed, out);
        }
        Stmt::For(s) => {
            detect_in_stmts(&s.body, ctx, int_mut, float_mut, sum_shadowed, out);
            detect_in_stmts(&s.orelse, ctx, int_mut, float_mut, sum_shadowed, out);
        }
        Stmt::With(s) => detect_in_stmts(&s.body, ctx, int_mut, float_mut, sum_shadowed, out),
        Stmt::Try(s) => {
            detect_in_stmts(&s.body, ctx, int_mut, float_mut, sum_shadowed, out);
            detect_in_stmts(&s.orelse, ctx, int_mut, float_mut, sum_shadowed, out);
            detect_in_stmts(&s.finalbody, ctx, int_mut, float_mut, sum_shadowed, out);
            for h in &s.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                detect_in_stmts(&h.body, ctx, int_mut, float_mut, sum_shadowed, out);
            }
        }
        Stmt::Match(s) => {
            for case in &s.cases {
                detect_in_stmts(&case.body, ctx, int_mut, float_mut, sum_shadowed, out);
            }
        }
        _ => {}
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

    fn rewrite(src: &str, pure: &[&str], min_size: u64) -> (String, ReductionStats) {
        let mut m = parse(src);
        let stats = rewrite_reduction_loops(&mut m, &pure_set(pure), min_size);
        (tyc_emit::emit_python(&m), stats)
    }

    #[test]
    fn rewrites_int_augassign_reduction() {
        let src = "\
@pure
def sq(n: int) -> int:
    return n * n

def run(xs: list[int]) -> int:
    mut total: int = 0
    for x in xs:
        total += sq(x)
    return total
";
        let (out, stats) = rewrite(src, &["sq"], 0);
        assert_eq!(
            stats.rewrites, 1,
            "int reduction should rewrite; got:\n{out}"
        );
        assert!(
            out.contains("total += sum(typhon_runtime.parallel.map_pure(lambda x: sq(x), xs))"),
            "unexpected lowering:\n{out}"
        );
        // The original for-loop is gone.
        assert!(
            !out.contains("for x in xs:"),
            "for loop should be replaced:\n{out}"
        );
    }

    #[test]
    fn rewrites_acc_eq_acc_plus_expr_form() {
        let src = "\
def run(xs: list[int]) -> int:
    mut total: int = 0
    for x in xs:
        total = total + x * x
    return total
";
        let (out, stats) = rewrite(src, &[], 0);
        assert_eq!(
            stats.rewrites, 1,
            "ACC = ACC + EXPR should rewrite; got:\n{out}"
        );
        assert!(
            out.contains("total += sum(typhon_runtime.parallel.map_pure"),
            "{out}"
        );
    }

    #[test]
    fn leaves_float_accumulator_alone() {
        let src = "\
def run(xs: list[float]) -> float:
    mut total: float = 0.0
    for x in xs:
        total += x
    return total
";
        let (_out, stats) = rewrite(src, &[], 0);
        assert_eq!(stats.rewrites, 0, "float accumulator must never rewrite");
    }

    #[test]
    fn leaves_untyped_accumulator_alone() {
        // No `mut total: int` declaration in scope.
        let src = "\
def run(xs: list[int]) -> int:
    total = 0
    for x in xs:
        total += x
    return total
";
        let (_out, stats) = rewrite(src, &[], 0);
        assert_eq!(
            stats.rewrites, 0,
            "reduction requires a `mut NAME: int` accumulator"
        );
    }

    #[test]
    fn leaves_multi_statement_body_alone() {
        let src = "\
def run(xs: list[int]) -> int:
    mut total: int = 0
    for x in xs:
        total += x
        print(x)
    return total
";
        let (_out, stats) = rewrite(src, &[], 0);
        assert_eq!(stats.rewrites, 0, "body must be exactly one statement");
    }

    #[test]
    fn leaves_impure_expr_alone() {
        // `io(x)` isn't in the pure set.
        let src = "\
def run(xs: list[int]) -> int:
    mut total: int = 0
    for x in xs:
        total += io(x)
    return total
";
        let (_out, stats) = rewrite(src, &[], 0);
        assert_eq!(stats.rewrites, 0, "impure element must veto");
    }

    #[test]
    fn leaves_reduction_with_break_alone() {
        // A `break` forces a multi-statement body (if + break), so the
        // exactly-one-statement rule already rejects it.
        let src = "\
def run(xs: list[int]) -> int:
    mut total: int = 0
    for x in xs:
        if x < 0:
            break
        total += x
    return total
";
        let (_out, stats) = rewrite(src, &[], 0);
        assert_eq!(stats.rewrites, 0, "a loop with break must not rewrite");
    }

    #[test]
    fn leaves_reduction_with_else_alone() {
        let src = "\
def run(xs: list[int]) -> int:
    mut total: int = 0
    for x in xs:
        total += x
    else:
        total += 1
    return total
";
        let (_out, stats) = rewrite(src, &[], 0);
        assert_eq!(stats.rewrites, 0, "a for/else loop must not rewrite");
    }

    #[test]
    fn leaves_acc_referencing_iter_alone() {
        // `total` appears in the iterable — not loop-invariant w.r.t. the
        // accumulator, so the rewrite is suppressed.
        let src = "\
def run() -> int:
    mut total: int = 0
    for x in range(total):
        total += x
    return total
";
        let (_out, stats) = rewrite(src, &[], 0);
        assert_eq!(
            stats.rewrites, 0,
            "iterable referencing the accumulator must veto"
        );
    }

    #[test]
    fn min_size_suppresses_short_literal_iters() {
        let src = "\
def run() -> int:
    mut total: int = 0
    for x in [1, 2, 3]:
        total += x
    return total
";
        let (_out, stats) = rewrite(src, &[], 64);
        assert_eq!(stats.rewrites, 0, "short literal iterable below threshold");
    }

    #[test]
    fn detect_reports_int_and_float_reductions() {
        let src = "\
def run(xs: list[int], ys: list[float]) -> None:
    mut isum: int = 0
    for x in xs:
        isum += x
    mut fsum: float = 0.0
    for y in ys:
        fsum += y
";
        let m = parse(src);
        let hits = detect_reduction_loops(&m, &pure_set(&[]), 0);
        assert_eq!(hits.len(), 2, "both reductions detected");
        assert!(hits.iter().any(|h| !h.is_float && h.acc == "isum"));
        assert!(hits.iter().any(|h| h.is_float && h.acc == "fsum"));
    }

    #[test]
    fn same_name_int_and_float_accumulators_do_not_cross_contaminate() {
        // Regression: `collect_typed_mut_names` used to flatten every
        // `mut NAME: <ann>` in the module into one set, so fn A's
        // `mut total: int` made fn B's same-named `mut total: float` loop look
        // int-typed and it was rewritten into `sum(map_pure(...))` — reordering
        // float addition and changing results. A must still rewrite; B must not.
        let src = "\
def a(xs: list[int]) -> int:
    mut total: int = 0
    for x in xs:
        total += x
    return total

def b(ys: list[float]) -> float:
    mut total: float = 0.0
    for y in ys:
        total += y
    return total
";
        let (out, stats) = rewrite(src, &[], 0);
        assert_eq!(
            stats.rewrites, 1,
            "only A's int loop may rewrite; B's float loop must not:\n{out}"
        );
        // A's int accumulator was parallelised …
        assert!(
            out.contains("total += sum(typhon_runtime.parallel.map_pure(lambda x: x, xs))"),
            "A's int reduction should be parallelised:\n{out}"
        );
        // … and B's float accumulator is left as a plain sequential loop.
        assert!(
            out.contains("for y in ys:"),
            "B's float loop must be left untouched (no float reordering):\n{out}"
        );
    }

    #[test]
    fn same_name_untyped_accumulator_does_not_borrow_int_typing() {
        // fn A declares `mut total: int`; fn B uses a same-named accumulator
        // with NO annotation. B must not borrow A's int typing across scopes —
        // only A rewrites.
        let src = "\
def a(xs: list[int]) -> int:
    mut total: int = 0
    for x in xs:
        total += x
    return total

def b(xs: list[int]) -> int:
    total = 0
    for x in xs:
        total += x
    return total
";
        let (out, stats) = rewrite(src, &[], 0);
        assert_eq!(
            stats.rewrites, 1,
            "only A rewrites; B's unannotated accumulator is ineligible:\n{out}"
        );
    }

    #[test]
    fn module_level_sum_binding_suppresses_rewrite() {
        // The rewrite emits a bare `sum(...)` call; a module-level `def sum`
        // would capture it and change behaviour, so the rewrite must not fire.
        let src = "\
def sum(xs: list[int]) -> int:
    return 0

def run(xs: list[int]) -> int:
    mut total: int = 0
    for x in xs:
        total += x
    return total
";
        let (out, stats) = rewrite(src, &[], 0);
        assert_eq!(
            stats.rewrites, 0,
            "a module-level `sum` binding must suppress the rewrite:\n{out}"
        );
        assert!(
            out.contains("for x in xs:"),
            "the loop must be left untouched:\n{out}"
        );
    }

    #[test]
    fn local_sum_binding_suppresses_rewrite() {
        // Same for a local binding of `sum` in the enclosing function.
        let src = "\
def run(xs: list[int]) -> int:
    let sum: int = 3
    mut total: int = 0
    for x in xs:
        total += x
    return total + sum
";
        let (out, stats) = rewrite(src, &[], 0);
        assert_eq!(
            stats.rewrites, 0,
            "a local `sum` binding must suppress the rewrite:\n{out}"
        );
    }

    #[test]
    fn parameter_named_sum_suppresses_rewrite() {
        let src = "\
def run(xs: list[int], sum: int) -> int:
    mut total: int = 0
    for x in xs:
        total += x
    return total + sum
";
        let (out, stats) = rewrite(src, &[], 0);
        assert_eq!(
            stats.rewrites, 0,
            "a parameter named `sum` must suppress the rewrite:\n{out}"
        );
    }

    #[test]
    fn sum_used_but_never_bound_still_rewrites() {
        // Merely *calling* the builtin `sum` (a Load, not a binding) must not
        // over-suppress the rewrite.
        let src = "\
def run(xs: list[int], ys: list[int]) -> int:
    let base: int = sum(ys)
    mut total: int = 0
    for x in xs:
        total += x
    return total + base
";
        let (out, stats) = rewrite(src, &[], 0);
        assert_eq!(
            stats.rewrites, 1,
            "a Load-context use of `sum` must not suppress the rewrite:\n{out}"
        );
        assert!(
            out.contains("total += sum(typhon_runtime.parallel.map_pure"),
            "expected the reduction lowering:\n{out}"
        );
    }

    #[test]
    fn same_name_int_accumulators_in_separate_functions_both_rewrite() {
        // The per-scope resolution must not over-reject: two *independent* `int`
        // accumulators that happen to share a name each rewrite in their own
        // function.
        let src = "\
def a(xs: list[int]) -> int:
    mut total: int = 0
    for x in xs:
        total += x
    return total

def b(ys: list[int]) -> int:
    mut total: int = 0
    for y in ys:
        total += y
    return total
";
        let (out, stats) = rewrite(src, &[], 0);
        assert_eq!(
            stats.rewrites, 2,
            "both same-named int accumulators should rewrite in their own scope:\n{out}"
        );
    }
}
