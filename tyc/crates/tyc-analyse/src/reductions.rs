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
    let int_mut = collect_typed_mut_names(module, "int");
    let ctx = RewriteCtx::new(pure_callees, min_size, &captures);
    let mut stats = ReductionStats::default();
    rewrite_stmts(&mut module.body, &ctx, &int_mut, &mut stats);
    stats
}

fn rewrite_stmts(
    body: &mut [Stmt],
    ctx: &RewriteCtx<'_>,
    int_mut: &HashSet<String>,
    stats: &mut ReductionStats,
) {
    for stmt in body.iter_mut() {
        // Recurse first so nested loops inside a matched loop's *sibling*
        // blocks are considered (a matched leaf loop has no nested statements
        // to recurse into anyway).
        recurse_children(stmt, ctx, int_mut, stats);
        if let Stmt::For(f) = stmt {
            if let Some(m) = match_reduction_loop(f, ctx, int_mut) {
                if !under_min_size(&m.iter, ctx.min_size) {
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
    int_mut: &HashSet<String>,
    stats: &mut ReductionStats,
) {
    match stmt {
        Stmt::FunctionDef(f) => rewrite_stmts(&mut f.body, ctx, int_mut, stats),
        Stmt::ClassDef(c) => rewrite_stmts(&mut c.body, ctx, int_mut, stats),
        Stmt::If(s) => {
            rewrite_stmts(&mut s.body, ctx, int_mut, stats);
            for clause in &mut s.elif_else_clauses {
                rewrite_stmts(&mut clause.body, ctx, int_mut, stats);
            }
        }
        Stmt::While(s) => {
            rewrite_stmts(&mut s.body, ctx, int_mut, stats);
            rewrite_stmts(&mut s.orelse, ctx, int_mut, stats);
        }
        Stmt::For(s) => {
            rewrite_stmts(&mut s.body, ctx, int_mut, stats);
            rewrite_stmts(&mut s.orelse, ctx, int_mut, stats);
        }
        Stmt::With(s) => rewrite_stmts(&mut s.body, ctx, int_mut, stats),
        Stmt::Try(s) => {
            rewrite_stmts(&mut s.body, ctx, int_mut, stats);
            rewrite_stmts(&mut s.orelse, ctx, int_mut, stats);
            rewrite_stmts(&mut s.finalbody, ctx, int_mut, stats);
            for h in &mut s.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                rewrite_stmts(&mut h.body, ctx, int_mut, stats);
            }
        }
        Stmt::Match(s) => {
            for case in &mut s.cases {
                rewrite_stmts(&mut case.body, ctx, int_mut, stats);
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

/// Collect every name declared `mut NAME: <ann>` at any scope, where `<ann>`
/// is the bare type name `ann` (`"int"` / `"float"`).
pub(crate) fn collect_typed_mut_names(module: &ModModule, ann: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    collect_typed_mut_into(&module.body, ann, &mut out);
    out
}

fn collect_typed_mut_into(body: &[Stmt], ann: &str, out: &mut HashSet<String>) {
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
            Stmt::FunctionDef(f) => collect_typed_mut_into(&f.body, ann, out),
            Stmt::ClassDef(c) => collect_typed_mut_into(&c.body, ann, out),
            Stmt::If(s) => {
                collect_typed_mut_into(&s.body, ann, out);
                for clause in &s.elif_else_clauses {
                    collect_typed_mut_into(&clause.body, ann, out);
                }
            }
            Stmt::While(s) => {
                collect_typed_mut_into(&s.body, ann, out);
                collect_typed_mut_into(&s.orelse, ann, out);
            }
            Stmt::For(s) => {
                collect_typed_mut_into(&s.body, ann, out);
                collect_typed_mut_into(&s.orelse, ann, out);
            }
            Stmt::With(s) => collect_typed_mut_into(&s.body, ann, out),
            Stmt::Try(s) => {
                collect_typed_mut_into(&s.body, ann, out);
                collect_typed_mut_into(&s.orelse, ann, out);
                collect_typed_mut_into(&s.finalbody, ann, out);
                for h in &s.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    collect_typed_mut_into(&h.body, ann, out);
                }
            }
            Stmt::Match(s) => {
                for case in &s.cases {
                    collect_typed_mut_into(&case.body, ann, out);
                }
            }
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
    let int_mut = collect_typed_mut_names(module, "int");
    let float_mut = collect_typed_mut_names(module, "float");
    let ctx = RewriteCtx::new(pure_callees, min_size, &captures);
    let mut out = Vec::new();
    detect_in_stmts(&module.body, &ctx, &int_mut, &float_mut, &mut out);
    out
}

fn detect_in_stmts(
    body: &[Stmt],
    ctx: &RewriteCtx<'_>,
    int_mut: &HashSet<String>,
    float_mut: &HashSet<String>,
    out: &mut Vec<ReductionHit>,
) {
    for stmt in body {
        detect_children(stmt, ctx, int_mut, float_mut, out);
        if let Stmt::For(f) = stmt {
            // int accumulator → eligible for the rewrite.
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
            // float accumulator → matches everything but the int annotation.
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
    out: &mut Vec<ReductionHit>,
) {
    match stmt {
        Stmt::FunctionDef(f) => detect_in_stmts(&f.body, ctx, int_mut, float_mut, out),
        Stmt::ClassDef(c) => detect_in_stmts(&c.body, ctx, int_mut, float_mut, out),
        Stmt::If(s) => {
            detect_in_stmts(&s.body, ctx, int_mut, float_mut, out);
            for clause in &s.elif_else_clauses {
                detect_in_stmts(&clause.body, ctx, int_mut, float_mut, out);
            }
        }
        Stmt::While(s) => {
            detect_in_stmts(&s.body, ctx, int_mut, float_mut, out);
            detect_in_stmts(&s.orelse, ctx, int_mut, float_mut, out);
        }
        Stmt::For(s) => {
            detect_in_stmts(&s.body, ctx, int_mut, float_mut, out);
            detect_in_stmts(&s.orelse, ctx, int_mut, float_mut, out);
        }
        Stmt::With(s) => detect_in_stmts(&s.body, ctx, int_mut, float_mut, out),
        Stmt::Try(s) => {
            detect_in_stmts(&s.body, ctx, int_mut, float_mut, out);
            detect_in_stmts(&s.orelse, ctx, int_mut, float_mut, out);
            detect_in_stmts(&s.finalbody, ctx, int_mut, float_mut, out);
            for h in &s.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                detect_in_stmts(&h.body, ctx, int_mut, float_mut, out);
            }
        }
        Stmt::Match(s) => {
            for case in &s.cases {
                detect_in_stmts(&case.body, ctx, int_mut, float_mut, out);
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
}
