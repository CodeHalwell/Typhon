//! Auto-gather inference (Phase 4).
//!
//! Detects straight-line runs of independent `await` calls inside an
//! `async def` body and rewrites them into an `asyncio.TaskGroup`
//! block so they execute concurrently instead of sequentially.
//!
//! Conservative on purpose: a run is only rewritten when every member is
//!
//! 1. A plain `name = await callee(args)` assignment (no augmented
//!    targets, attribute targets, or annotation noise).
//! 2. The callee is a bare-name reference whose target is in
//!    `eligible_async_fns` — i.e. an `async def` declared in the same
//!    module that the caller has flagged as gather-safe.
//! 3. Independent of earlier members: no argument of statement `j`
//!    references any name bound by statements `i < j` in the run.
//!
//! The rewrite preserves the order of the binding sites, only the
//! awaits themselves run concurrently. Statements that don't match
//! the candidate shape end any in-progress run, so a single impure
//! line between two awaits suppresses the rewrite.
//!
//! Imports: the rewrite emits `asyncio.TaskGroup` references against
//! the bare `asyncio` module name. The desugar pass detects this and
//! injects `import asyncio` if missing, so no extra wiring is needed
//! here.

use std::collections::HashSet;

use ruff_python_ast::{
    name::Name, Arguments, AtomicNodeIndex, ExceptHandler, Expr, ExprAttribute, ExprCall,
    ExprContext, ExprName, Identifier, Keyword, ModModule, Stmt, StmtAssign, StmtWith, WithItem,
};
use ruff_text_size::TextRange;

/// Summary of what the pass rewrote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AutoGatherStats {
    /// Number of distinct gather-rewrites performed across the module.
    pub rewrites: usize,
    /// Total number of await statements folded into those rewrites.
    pub awaits_folded: usize,
}

/// Rewrite eligible await-runs into `asyncio.TaskGroup` blocks.
///
/// `eligible_async_fns` is the set of bare-name targets the caller has
/// proven safe to gather (typically: every `async def` carrying the
/// `@gatherable` decorator — see [`collect_gatherable_async_fn_names`]).
pub fn rewrite_auto_gather(
    module: &mut ModModule,
    eligible_async_fns: &HashSet<String>,
) -> AutoGatherStats {
    let mut stats = AutoGatherStats::default();
    // Seed the counter past any pre-existing `__typhon_autogather_*` name
    // referenced in the module so the synthesized names never collide with
    // user code or a previous rewrite that left identifiers behind. The
    // scan is O(N) over the AST and only runs once per module.
    let mut counter: usize = next_safe_counter(module);
    let body = std::mem::take(&mut module.body);
    module.body = rewrite_stmts(
        body,
        eligible_async_fns,
        /* inside_async */ false,
        &mut counter,
        &mut stats,
    );
    stats
}

/// Walk `module` and return a counter value that is strictly greater than
/// any `__typhon_autogather_(tg|task)_N_` numeric suffix already in scope.
/// Guards the synthesized identifiers in [`make_autogather_block`]
/// against collisions with user names or earlier rewrites.
fn next_safe_counter(module: &ModModule) -> usize {
    let mut highest_seen: Option<usize> = None;
    scan_stmts_for_existing_counter(&module.body, &mut highest_seen);
    highest_seen.map(|n| n + 1).unwrap_or(0)
}

fn scan_stmts_for_existing_counter(stmts: &[Stmt], highest: &mut Option<usize>) {
    for stmt in stmts {
        scan_stmt_for_existing_counter(stmt, highest);
    }
}

fn scan_stmt_for_existing_counter(stmt: &Stmt, highest: &mut Option<usize>) {
    use Stmt::*;
    match stmt {
        FunctionDef(f) => scan_stmts_for_existing_counter(&f.body, highest),
        ClassDef(c) => scan_stmts_for_existing_counter(&c.body, highest),
        If(s) => {
            scan_stmts_for_existing_counter(&s.body, highest);
            for clause in &s.elif_else_clauses {
                scan_stmts_for_existing_counter(&clause.body, highest);
            }
        }
        While(s) => {
            scan_stmts_for_existing_counter(&s.body, highest);
            scan_stmts_for_existing_counter(&s.orelse, highest);
        }
        For(s) => {
            scan_stmts_for_existing_counter(&s.body, highest);
            scan_stmts_for_existing_counter(&s.orelse, highest);
        }
        With(s) => scan_stmts_for_existing_counter(&s.body, highest),
        Try(s) => {
            scan_stmts_for_existing_counter(&s.body, highest);
            scan_stmts_for_existing_counter(&s.orelse, highest);
            scan_stmts_for_existing_counter(&s.finalbody, highest);
            for h in &s.handlers {
                let ExceptHandler::ExceptHandler(h) = h;
                scan_stmts_for_existing_counter(&h.body, highest);
            }
        }
        Match(s) => {
            for case in &s.cases {
                scan_stmts_for_existing_counter(&case.body, highest);
            }
        }
        Assign(a) => {
            for t in &a.targets {
                scan_expr_for_existing_counter(t, highest);
            }
            scan_expr_for_existing_counter(&a.value, highest);
        }
        AnnAssign(a) => {
            scan_expr_for_existing_counter(&a.target, highest);
            if let Some(v) = &a.value {
                scan_expr_for_existing_counter(v, highest);
            }
        }
        Expr(e) => scan_expr_for_existing_counter(&e.value, highest),
        _ => {}
    }
}

fn scan_expr_for_existing_counter(expr: &Expr, highest: &mut Option<usize>) {
    if let Expr::Name(n) = expr {
        if let Some(num) = parse_autogather_suffix(n.id.as_str()) {
            *highest = Some(highest.map_or(num, |h| h.max(num)));
        }
    }
    // We don't recurse into every expression kind: the only place the
    // generated names appear in source-form is as bare `Name` references
    // (assignment targets and assignment RHS). Catching them at those
    // sites is sufficient because the rewrite only ever produces those
    // forms.
}

/// Parse the trailing integer in a `__typhon_autogather_tg_<N>__` or
/// `__typhon_autogather_task_<N>_<i>__` identifier. Returns the `<N>` so
/// the seeder can step past it. Anything that doesn't match returns
/// `None`.
fn parse_autogather_suffix(name: &str) -> Option<usize> {
    const TG_PREFIX: &str = "__typhon_autogather_tg_";
    const TASK_PREFIX: &str = "__typhon_autogather_task_";
    let body = if let Some(rest) = name.strip_prefix(TG_PREFIX) {
        rest.strip_suffix("__")?.to_string()
    } else if let Some(rest) = name.strip_prefix(TASK_PREFIX) {
        // Strip the `_<i>__` task-index suffix.
        let no_trailing = rest.strip_suffix("__")?;
        let (head, _) = no_trailing.rsplit_once('_')?;
        head.to_string()
    } else {
        return None;
    };
    body.parse().ok()
}

// ── walker ───────────────────────────────────────────────────────────────────

fn rewrite_stmts(
    body: Vec<Stmt>,
    eligible: &HashSet<String>,
    inside_async: bool,
    counter: &mut usize,
    stats: &mut AutoGatherStats,
) -> Vec<Stmt> {
    let mut out: Vec<Stmt> = Vec::with_capacity(body.len());
    // Move into an iterator so we can take ownership of each stmt without
    // cloning. We need lookahead for run-detection, so peek at the upcoming
    // slice via an index into the source `Vec`.
    let mut i = 0;
    while i < body.len() {
        if inside_async {
            let run = collect_run(&body, i, eligible);
            if run.len() >= 2 {
                // Emit the synthesized async-with + result-extracts directly
                // into the enclosing block; no `if True:` wrapper so the
                // emitted Python stays at the same indent as the awaits it
                // replaces.
                let synthesized = make_autogather_block(&run, counter);
                out.extend(synthesized);
                stats.rewrites += 1;
                stats.awaits_folded += run.len();
                i += run.len();
                continue;
            }
        }
        out.push(recurse_stmt(
            body[i].clone(),
            eligible,
            inside_async,
            counter,
            stats,
        ));
        i += 1;
    }
    out
}

fn recurse_stmt(
    stmt: Stmt,
    eligible: &HashSet<String>,
    inside_async: bool,
    counter: &mut usize,
    stats: &mut AutoGatherStats,
) -> Stmt {
    match stmt {
        Stmt::FunctionDef(mut f) => {
            // A nested `async def` flips the walker back into async scope;
            // a `def` inside an `async def` flips it off.
            let body_async = f.is_async;
            f.body = rewrite_stmts(f.body, eligible, body_async, counter, stats);
            Stmt::FunctionDef(f)
        }
        Stmt::ClassDef(mut c) => {
            // Methods inside a class body run their own walk; whether they're
            // async is determined per-method (handled by the FunctionDef arm
            // above when we recurse into a method).
            c.body = rewrite_stmts(c.body, eligible, false, counter, stats);
            Stmt::ClassDef(c)
        }
        Stmt::If(mut s) => {
            s.body = rewrite_stmts(s.body, eligible, inside_async, counter, stats);
            for clause in s.elif_else_clauses.iter_mut() {
                clause.body = rewrite_stmts(
                    std::mem::take(&mut clause.body),
                    eligible,
                    inside_async,
                    counter,
                    stats,
                );
            }
            Stmt::If(s)
        }
        Stmt::While(mut s) => {
            s.body = rewrite_stmts(s.body, eligible, inside_async, counter, stats);
            s.orelse = rewrite_stmts(s.orelse, eligible, inside_async, counter, stats);
            Stmt::While(s)
        }
        Stmt::For(mut s) => {
            s.body = rewrite_stmts(s.body, eligible, inside_async, counter, stats);
            s.orelse = rewrite_stmts(s.orelse, eligible, inside_async, counter, stats);
            Stmt::For(s)
        }
        Stmt::With(mut s) => {
            s.body = rewrite_stmts(s.body, eligible, inside_async, counter, stats);
            Stmt::With(s)
        }
        Stmt::Try(mut s) => {
            s.body = rewrite_stmts(s.body, eligible, inside_async, counter, stats);
            for h in s.handlers.iter_mut() {
                let ExceptHandler::ExceptHandler(h) = h;
                h.body = rewrite_stmts(
                    std::mem::take(&mut h.body),
                    eligible,
                    inside_async,
                    counter,
                    stats,
                );
            }
            s.orelse = rewrite_stmts(s.orelse, eligible, inside_async, counter, stats);
            s.finalbody = rewrite_stmts(s.finalbody, eligible, inside_async, counter, stats);
            Stmt::Try(s)
        }
        Stmt::Match(mut s) => {
            for case in s.cases.iter_mut() {
                case.body = rewrite_stmts(
                    std::mem::take(&mut case.body),
                    eligible,
                    inside_async,
                    counter,
                    stats,
                );
            }
            Stmt::Match(s)
        }
        other => other,
    }
}

// ── candidate detection ──────────────────────────────────────────────────────

/// A single member of a candidate gather run.
#[derive(Debug)]
struct Candidate {
    bind: String,
    call_func: String,
    args: Box<[Expr]>,
    keywords: Box<[Keyword]>,
    call_range: TextRange,
}

/// Scan `body[start..]` and collect the longest prefix that forms a safe
/// gather run.
fn collect_run(body: &[Stmt], start: usize, eligible: &HashSet<String>) -> Vec<Candidate> {
    let mut run: Vec<Candidate> = Vec::new();
    let mut bound: HashSet<String> = HashSet::new();
    for stmt in &body[start..] {
        let Some(cand) = parse_candidate(stmt, eligible) else {
            break;
        };
        if call_uses_any(&cand.args, &cand.keywords, &bound) {
            break;
        }
        if bound.contains(&cand.bind) {
            // Shadowing within the same run would change semantics of later
            // result extraction; bail.
            break;
        }
        bound.insert(cand.bind.clone());
        run.push(cand);
    }
    run
}

/// Match `name = await CALLEE(args)` where CALLEE is a bare name in
/// `eligible`. Returns the deconstructed candidate or `None`.
fn parse_candidate(stmt: &Stmt, eligible: &HashSet<String>) -> Option<Candidate> {
    let assign = match stmt {
        Stmt::Assign(a) => a,
        _ => return None,
    };
    if assign.targets.len() != 1 {
        return None;
    }
    let bind = match &assign.targets[0] {
        Expr::Name(n) if matches!(n.ctx, ExprContext::Store) => n.id.as_str().to_owned(),
        _ => return None,
    };
    let await_expr = match &*assign.value {
        Expr::Await(a) => a,
        _ => return None,
    };
    let call = match &*await_expr.value {
        Expr::Call(c) => c,
        _ => return None,
    };
    let call_func = match &*call.func {
        Expr::Name(n) if eligible.contains(n.id.as_str()) => n.id.as_str().to_owned(),
        _ => return None,
    };
    Some(Candidate {
        bind,
        call_func,
        args: call.arguments.args.clone(),
        keywords: call.arguments.keywords.clone(),
        call_range: call.range,
    })
}

/// `true` if any expression in the call references a name in `bound`.
fn call_uses_any(args: &[Expr], keywords: &[Keyword], bound: &HashSet<String>) -> bool {
    args.iter().any(|e| expr_uses_any(e, bound))
        || keywords.iter().any(|k| expr_uses_any(&k.value, bound))
}

fn expr_uses_any(expr: &Expr, bound: &HashSet<String>) -> bool {
    match expr {
        Expr::Name(n) => bound.contains(n.id.as_str()),
        Expr::Call(c) => {
            expr_uses_any(&c.func, bound)
                || c.arguments.args.iter().any(|e| expr_uses_any(e, bound))
                || c.arguments
                    .keywords
                    .iter()
                    .any(|k| expr_uses_any(&k.value, bound))
        }
        Expr::Attribute(a) => expr_uses_any(&a.value, bound),
        Expr::Subscript(s) => expr_uses_any(&s.value, bound) || expr_uses_any(&s.slice, bound),
        Expr::Slice(s) => {
            s.lower.as_deref().is_some_and(|e| expr_uses_any(e, bound))
                || s.upper.as_deref().is_some_and(|e| expr_uses_any(e, bound))
                || s.step.as_deref().is_some_and(|e| expr_uses_any(e, bound))
        }
        Expr::BinOp(b) => expr_uses_any(&b.left, bound) || expr_uses_any(&b.right, bound),
        Expr::UnaryOp(u) => expr_uses_any(&u.operand, bound),
        Expr::BoolOp(b) => b.values.iter().any(|e| expr_uses_any(e, bound)),
        Expr::Compare(c) => {
            expr_uses_any(&c.left, bound) || c.comparators.iter().any(|e| expr_uses_any(e, bound))
        }
        Expr::If(i) => {
            expr_uses_any(&i.test, bound)
                || expr_uses_any(&i.body, bound)
                || expr_uses_any(&i.orelse, bound)
        }
        Expr::Lambda(_) => true, // conservative: don't peek into lambda bodies
        Expr::Tuple(t) => t.elts.iter().any(|e| expr_uses_any(e, bound)),
        Expr::List(l) => l.elts.iter().any(|e| expr_uses_any(e, bound)),
        Expr::Set(s) => s.elts.iter().any(|e| expr_uses_any(e, bound)),
        Expr::Dict(d) => d.items.iter().any(|item| {
            item.key.as_ref().is_some_and(|k| expr_uses_any(k, bound))
                || expr_uses_any(&item.value, bound)
        }),
        Expr::Starred(s) => expr_uses_any(&s.value, bound),
        Expr::Await(a) => expr_uses_any(&a.value, bound),
        // f-strings and t-strings in ruff don't expose the embedded
        // expressions as plain `Expr`s on the outside; an interpolation
        // referencing a bound name appears inside `FStringValue::elements()`.
        // Conservatively assume an interpolation may reference any bound
        // name — return true so the run is broken whenever the candidate
        // includes an f/t-string argument.
        Expr::FString(_) | Expr::TString(_) => true,
        // Walrus `:=` reads its RHS and writes its target; both can reference
        // earlier-bound names (`b = await fb((x := a + 1) + x)` reads `a`).
        Expr::Named(n) => expr_uses_any(&n.value, bound) || expr_uses_any(&n.target, bound),
        // Comprehensions and generators introduce their own scope, but the
        // first generator's `iter` is evaluated in the *enclosing* scope and
        // any later `iter`/`ifs`/`elt`/`key`/`value` can reference outer
        // names too. We walk every sub-expression conservatively: shadowing
        // by a comprehension's own target only ever produces a false
        // positive (we'd refuse to gather a run we could safely gather),
        // never a false negative, which is the safe direction.
        Expr::ListComp(c) => {
            expr_uses_any(&c.elt, bound) || comp_generators_use_any(&c.generators, bound)
        }
        Expr::SetComp(c) => {
            expr_uses_any(&c.elt, bound) || comp_generators_use_any(&c.generators, bound)
        }
        Expr::Generator(g) => {
            expr_uses_any(&g.elt, bound) || comp_generators_use_any(&g.generators, bound)
        }
        Expr::DictComp(c) => {
            c.key.as_deref().is_some_and(|k| expr_uses_any(k, bound))
                || expr_uses_any(&c.value, bound)
                || comp_generators_use_any(&c.generators, bound)
        }
        Expr::Yield(y) => y.value.as_deref().is_some_and(|e| expr_uses_any(e, bound)),
        Expr::YieldFrom(y) => expr_uses_any(&y.value, bound),
        // Constants, ellipsis: never reference bound names.
        _ => false,
    }
}

fn comp_generators_use_any(
    generators: &[ruff_python_ast::Comprehension],
    bound: &HashSet<String>,
) -> bool {
    generators.iter().any(|g| {
        expr_uses_any(&g.iter, bound)
            || expr_uses_any(&g.target, bound)
            || g.ifs.iter().any(|e| expr_uses_any(e, bound))
    })
}

// ── synthesis ────────────────────────────────────────────────────────────────

/// Synthesize the statement sequence that replaces a candidate gather run.
/// Returns the `async with asyncio.TaskGroup() as tg: ...` block followed
/// by one `name = task.result()` extraction per candidate. The caller
/// extends its enclosing block with the result so we don't need a wrapper
/// statement (no `if True:` no-op block).
fn make_autogather_block(run: &[Candidate], counter: &mut usize) -> Vec<Stmt> {
    let id = *counter;
    *counter += 1;

    let tg_name = format!("__typhon_autogather_tg_{}__", id);
    let task_name = |i: usize| format!("__typhon_autogather_task_{}_{}__", id, i);

    // Body of the `async with` block: one `task_i = tg.create_task(callee(args))`
    // per candidate.
    let mut block_body: Vec<Stmt> = Vec::with_capacity(run.len());
    for (i, c) in run.iter().enumerate() {
        block_body.push(make_create_task_assign(&tg_name, &task_name(i), c));
    }

    let async_with = Stmt::With(StmtWith {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        is_async: true,
        items: vec![WithItem {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            context_expr: make_taskgroup_call(),
            optional_vars: Some(Box::new(name_store(&tg_name))),
        }],
        body: block_body,
    });

    let mut out: Vec<Stmt> = Vec::with_capacity(1 + run.len());
    out.push(async_with);
    for (i, c) in run.iter().enumerate() {
        out.push(make_result_extract(&c.bind, &task_name(i)));
    }
    out
}

fn name_load(name: &str) -> Expr {
    Expr::Name(ExprName {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        id: Name::new(name),
        ctx: ExprContext::Load,
    })
}

fn name_store(name: &str) -> Expr {
    Expr::Name(ExprName {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        id: Name::new(name),
        ctx: ExprContext::Store,
    })
}

fn make_taskgroup_call() -> Expr {
    // asyncio.TaskGroup()
    Expr::Call(ExprCall {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        func: Box::new(Expr::Attribute(ExprAttribute {
            node_index: AtomicNodeIndex::NONE,
            range: TextRange::default(),
            value: Box::new(name_load("asyncio")),
            attr: Identifier::new("TaskGroup", TextRange::default()),
            ctx: ExprContext::Load,
        })),
        arguments: Arguments {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            args: Box::new([]),
            keywords: Box::new([]),
        },
    })
}

fn make_create_task_assign(tg: &str, task: &str, c: &Candidate) -> Stmt {
    // task = tg.create_task(callee(args))
    let inner_call = Expr::Call(ExprCall {
        node_index: AtomicNodeIndex::NONE,
        range: c.call_range,
        func: Box::new(name_load(&c.call_func)),
        arguments: Arguments {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            args: c.args.clone(),
            keywords: c.keywords.clone(),
        },
    });
    let create_task = Expr::Call(ExprCall {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        func: Box::new(Expr::Attribute(ExprAttribute {
            node_index: AtomicNodeIndex::NONE,
            range: TextRange::default(),
            value: Box::new(name_load(tg)),
            attr: Identifier::new("create_task", TextRange::default()),
            ctx: ExprContext::Load,
        })),
        arguments: Arguments {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            args: Box::new([inner_call]),
            keywords: Box::new([]),
        },
    });
    Stmt::Assign(StmtAssign {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        targets: vec![name_store(task)],
        value: Box::new(create_task),
        mutability: None,
    })
}

fn make_result_extract(bind: &str, task: &str) -> Stmt {
    // bind = task.result()
    let result_call = Expr::Call(ExprCall {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        func: Box::new(Expr::Attribute(ExprAttribute {
            node_index: AtomicNodeIndex::NONE,
            range: TextRange::default(),
            value: Box::new(name_load(task)),
            attr: Identifier::new("result", TextRange::default()),
            ctx: ExprContext::Load,
        })),
        arguments: Arguments {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            args: Box::new([]),
            keywords: Box::new([]),
        },
    });
    Stmt::Assign(StmtAssign {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        targets: vec![name_store(bind)],
        value: Box::new(result_call),
        mutability: None,
    })
}

// ── module-level fn collection ──────────────────────────────────────────────

/// Collect the names of every top-level `async def` in `module` carrying
/// the `@gatherable` decorator. The build pipeline uses this as the
/// `eligible_async_fns` set when `[strictness] auto-gather = true`.
///
/// Requiring an explicit decorator (rather than treating every async
/// function in the module as gather-safe) is the safety boundary: an
/// async function that looks independent by return-value data flow can
/// still rely on ordering through I/O, shared state, locks, or rate
/// limits. The user attests "this function is safe to run concurrently
/// with peers in a gather block" by writing `@gatherable`; we never
/// infer it.
pub fn collect_gatherable_async_fn_names(module: &ModModule) -> HashSet<String> {
    let mut out = HashSet::new();
    for stmt in &module.body {
        if let Stmt::FunctionDef(f) = stmt {
            if f.is_async && has_gatherable_decorator(&f.decorator_list) {
                out.insert(f.name.as_str().to_owned());
            }
        }
    }
    out
}

fn has_gatherable_decorator(decorators: &[ruff_python_ast::Decorator]) -> bool {
    decorators.iter().any(|d| match &d.expression {
        Expr::Name(n) => n.id.as_str() == "gatherable",
        // `@gatherable(...)` — accept call-form too so future options
        // can ride on the same decorator without breaking existing code.
        Expr::Call(c) => {
            matches!(&*c.func, Expr::Name(n) if n.id.as_str() == "gatherable")
        }
        _ => false,
    })
}

// ── missed-gather detection (advice diagnostic, no rewrite) ──────────────────

/// One run of adjacent awaits that would have folded into a TaskGroup
/// if every callee carried `@gatherable`. Reported via
/// [`TycError::AutoGatherMissed`] so users see WHY auto-gather silently
/// left a run sequential.
#[derive(Debug, Clone)]
pub struct MissedGather {
    /// The first same-module async callee in the run that is missing
    /// the decorator. We only mention one in the diagnostic to keep
    /// the message terse; users add the decorator iteratively.
    pub missing_callee: String,
    /// Byte range of the first `await CALLEE(...)` call in the run, so
    /// the diagnostic anchors at the start of the would-be gather
    /// block.
    pub call_range: TextRange,
}

/// Scan `module` for runs of 2+ adjacent `name = await CALLEE(...)`
/// assignments inside `async def` bodies where:
///
/// - every callee is a bare-name reference to a same-module `async def`
///   (so auto-gather *could* fold this run if decorators were in
///   place),
/// - at least one callee lacks `@gatherable`,
/// - the run is independent (later awaits don't consume earlier
///   bindings),
/// - no later assignment in the run shadows an earlier one.
///
/// Returns one [`MissedGather`] per run, in source order. The caller
/// is responsible for converting these into [`TycError::AutoGatherMissed`]
/// advice diagnostics with the right source path / text.
///
/// Skipped when `[strictness] auto-gather` is off — without the opt-in,
/// the user almost certainly doesn't want the nudge.
pub fn detect_missed_gathers(module: &ModModule) -> Vec<MissedGather> {
    let decorated = collect_gatherable_async_fn_names(module);
    let local_async = collect_local_async_fn_names(module);
    // Anything in `local_async` but not in `decorated` is a missed
    // opportunity; that's the eligible set for "could have been
    // gathered if decorated".
    let eligible_if_decorated: HashSet<String> =
        local_async.iter().cloned().collect();
    let mut out = Vec::new();
    walk_missed_in_stmts(
        &module.body,
        &eligible_if_decorated,
        &decorated,
        /* inside_async */ false,
        &mut out,
    );
    out
}

fn collect_local_async_fn_names(module: &ModModule) -> HashSet<String> {
    let mut out = HashSet::new();
    for stmt in &module.body {
        if let Stmt::FunctionDef(f) = stmt {
            if f.is_async {
                out.insert(f.name.as_str().to_owned());
            }
        }
    }
    out
}

fn walk_missed_in_stmts(
    body: &[Stmt],
    eligible_if_decorated: &HashSet<String>,
    decorated: &HashSet<String>,
    inside_async: bool,
    out: &mut Vec<MissedGather>,
) {
    let mut i = 0;
    while i < body.len() {
        if inside_async {
            let run = collect_run(body, i, eligible_if_decorated);
            if run.len() >= 2 {
                let missing = run.iter().find(|c| !decorated.contains(&c.call_func));
                if let Some(m) = missing {
                    out.push(MissedGather {
                        missing_callee: m.call_func.clone(),
                        call_range: run[0].call_range,
                    });
                }
                i += run.len();
                continue;
            }
        }
        walk_missed_in_stmt(
            &body[i],
            eligible_if_decorated,
            decorated,
            inside_async,
            out,
        );
        i += 1;
    }
}

fn walk_missed_in_stmt(
    stmt: &Stmt,
    eligible_if_decorated: &HashSet<String>,
    decorated: &HashSet<String>,
    inside_async: bool,
    out: &mut Vec<MissedGather>,
) {
    match stmt {
        Stmt::FunctionDef(f) => walk_missed_in_stmts(
            &f.body,
            eligible_if_decorated,
            decorated,
            f.is_async,
            out,
        ),
        Stmt::ClassDef(c) => {
            walk_missed_in_stmts(&c.body, eligible_if_decorated, decorated, false, out);
        }
        Stmt::If(s) => {
            walk_missed_in_stmts(&s.body, eligible_if_decorated, decorated, inside_async, out);
            for clause in &s.elif_else_clauses {
                walk_missed_in_stmts(
                    &clause.body,
                    eligible_if_decorated,
                    decorated,
                    inside_async,
                    out,
                );
            }
        }
        Stmt::While(s) => {
            walk_missed_in_stmts(&s.body, eligible_if_decorated, decorated, inside_async, out);
            walk_missed_in_stmts(
                &s.orelse,
                eligible_if_decorated,
                decorated,
                inside_async,
                out,
            );
        }
        Stmt::For(s) => {
            walk_missed_in_stmts(&s.body, eligible_if_decorated, decorated, inside_async, out);
            walk_missed_in_stmts(
                &s.orelse,
                eligible_if_decorated,
                decorated,
                inside_async,
                out,
            );
        }
        Stmt::With(s) => {
            walk_missed_in_stmts(&s.body, eligible_if_decorated, decorated, inside_async, out);
        }
        Stmt::Try(s) => {
            walk_missed_in_stmts(&s.body, eligible_if_decorated, decorated, inside_async, out);
            for h in &s.handlers {
                let ExceptHandler::ExceptHandler(h) = h;
                walk_missed_in_stmts(
                    &h.body,
                    eligible_if_decorated,
                    decorated,
                    inside_async,
                    out,
                );
            }
            walk_missed_in_stmts(
                &s.orelse,
                eligible_if_decorated,
                decorated,
                inside_async,
                out,
            );
            walk_missed_in_stmts(
                &s.finalbody,
                eligible_if_decorated,
                decorated,
                inside_async,
                out,
            );
        }
        Stmt::Match(s) => {
            for case in &s.cases {
                walk_missed_in_stmts(
                    &case.body,
                    eligible_if_decorated,
                    decorated,
                    inside_async,
                    out,
                );
            }
        }
        _ => {}
    }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_module(src: &str) -> ModModule {
        tyc_syntax::parse_module(src)
            .expect("parse failed")
            .into_syntax()
    }

    fn render(module: &ModModule) -> String {
        tyc_emit::emit(module)
    }

    fn rewrite(src: &str) -> (String, AutoGatherStats) {
        let mut module = parse_module(src);
        let eligible = collect_gatherable_async_fn_names(&module);
        let stats = rewrite_auto_gather(&mut module, &eligible);
        (render(&module), stats)
    }

    #[test]
    fn two_independent_awaits_are_folded() {
        let src = "\
@gatherable
async def fetch_a() -> int:
    return 1

@gatherable
async def fetch_b() -> int:
    return 2

async def load() -> int:
    a = await fetch_a()
    b = await fetch_b()
    return a + b
";
        let (out, stats) = rewrite(src);
        assert_eq!(stats.rewrites, 1, "expected 1 rewrite; got: {stats:?}");
        assert_eq!(stats.awaits_folded, 2);
        assert!(
            out.contains("asyncio.TaskGroup"),
            "expected TaskGroup in output:\n{out}"
        );
        assert!(
            out.contains(".create_task("),
            "expected create_task calls:\n{out}"
        );
        assert!(
            out.contains(".result()"),
            "expected .result() calls:\n{out}"
        );
        assert!(
            !out.contains("a = await fetch_a"),
            "original sequential await should be gone:\n{out}"
        );
    }

    #[test]
    fn dependent_await_breaks_run() {
        // `b` depends on `a`, so they cannot run concurrently.
        let src = "\
@gatherable
async def fetch_a() -> int:
    return 1

@gatherable
async def fetch_b(x: int) -> int:
    return x

async def load() -> int:
    a = await fetch_a()
    b = await fetch_b(a)
    return a + b
";
        let (out, stats) = rewrite(src);
        assert_eq!(stats.rewrites, 0, "expected no rewrite; got: {stats:?}");
        assert!(
            out.contains("a = await fetch_a"),
            "should be untouched:\n{out}"
        );
    }

    #[test]
    fn comprehension_dependency_breaks_run() {
        // Regression for the gemini/Codex review: a comprehension that
        // closes over the first await's bound name `a` must NOT be
        // treated as independent. Without recursion into the
        // comprehension's `elt`/`iter`/`ifs`, the pass would happily
        // gather these and the rewritten code would `NameError` on `a`.
        let src = "\
@gatherable
async def fa() -> int:
    return 1

@gatherable
async def fb(xs: list[int]) -> int:
    return sum(xs)

async def load() -> int:
    a = await fa()
    b = await fb([a for _ in range(3)])
    return a + b
";
        let (_out, stats) = rewrite(src);
        assert_eq!(stats.rewrites, 0, "comprehension closure must break run");
    }

    #[test]
    fn walrus_dependency_breaks_run() {
        let src = "\
@gatherable
async def fa() -> int:
    return 1

@gatherable
async def fb(x: int) -> int:
    return x

async def load() -> int:
    a = await fa()
    b = await fb((y := a + 1))
    return a + b + y
";
        let (_out, stats) = rewrite(src);
        assert_eq!(stats.rewrites, 0, "walrus reading `a` must break run");
    }

    #[test]
    fn slice_dependency_breaks_run() {
        let src = "\
@gatherable
async def fa() -> int:
    return 1

@gatherable
async def fb(xs: list[int]) -> int:
    return 0

async def load() -> int:
    a = await fa()
    b = await fb([1, 2, 3][a:])
    return a + b
";
        let (_out, stats) = rewrite(src);
        assert_eq!(stats.rewrites, 0, "slice using `a` must break run");
    }

    #[test]
    fn single_await_not_rewritten() {
        let src = "\
@gatherable
async def fetch_a() -> int:
    return 1

async def load() -> int:
    a = await fetch_a()
    return a
";
        let (_out, stats) = rewrite(src);
        assert_eq!(stats.rewrites, 0);
    }

    #[test]
    fn awaits_outside_async_def_ignored() {
        // No async wrapper, even though the function names are async.
        // The walker only attempts to gather inside async scope; since
        // this is parsed as a single sync function, no rewrite happens.
        let src = "\
@gatherable
async def fetch_a() -> int:
    return 1

@gatherable
async def fetch_b() -> int:
    return 2

def load():
    a = await fetch_a()
    b = await fetch_b()
    return a + b
";
        let (_out, stats) = rewrite(src);
        assert_eq!(stats.rewrites, 0);
    }

    #[test]
    fn callee_not_in_eligible_set_breaks_run() {
        // `external_fetch` isn't in the module's @gatherable set, so the
        // walker treats it as opaque and doesn't gather.
        let src = "\
@gatherable
async def fetch_a() -> int:
    return 1

async def load() -> int:
    a = await fetch_a()
    b = await external_fetch()
    return a + b
";
        let (_out, stats) = rewrite(src);
        assert_eq!(stats.rewrites, 0);
    }

    #[test]
    fn undecorated_callee_is_ineligible() {
        // `fb` has no @gatherable decorator — auto-gather must skip it
        // even though it is an async def in the same module.
        let src = "\
@gatherable
async def fa() -> int:
    return 1

async def fb() -> int:
    return 2

async def load() -> int:
    a = await fa()
    b = await fb()
    return a + b
";
        let (_out, stats) = rewrite(src);
        assert_eq!(
            stats.rewrites, 0,
            "undecorated callee must NOT be folded even when others are"
        );
    }

    #[test]
    fn three_independent_awaits_fold_together() {
        let src = "\
@gatherable
async def fa() -> int:
    return 1
@gatherable
async def fb() -> int:
    return 2
@gatherable
async def fc() -> int:
    return 3

async def load() -> int:
    a = await fa()
    b = await fb()
    c = await fc()
    return a + b + c
";
        let (out, stats) = rewrite(src);
        assert_eq!(stats.rewrites, 1);
        assert_eq!(stats.awaits_folded, 3);
        assert!(
            out.matches(".create_task(").count() == 3,
            "expected 3 create_task calls:\n{out}"
        );
    }

    #[test]
    fn impure_statement_between_awaits_breaks_run() {
        let src = "\
@gatherable
async def fa() -> int:
    return 1
@gatherable
async def fb() -> int:
    return 2

async def load() -> int:
    a = await fa()
    print(\"between\")
    b = await fb()
    return a + b
";
        let (_out, stats) = rewrite(src);
        assert_eq!(stats.rewrites, 0);
    }

    #[test]
    fn nested_async_def_inside_sync_def_still_gathers() {
        // The inner `async def inner` switches the walker back into async
        // scope, so its body is eligible for gathering.
        let src = "\
@gatherable
async def fa() -> int:
    return 1
@gatherable
async def fb() -> int:
    return 2

def outer():
    async def inner() -> int:
        a = await fa()
        b = await fb()
        return a + b
    return inner
";
        let (_out, stats) = rewrite(src);
        assert_eq!(stats.rewrites, 1);
    }

    #[test]
    fn collect_gatherable_finds_only_decorated_async_defs() {
        let module = parse_module(
            "\
@gatherable
async def alpha() -> int:
    return 1

def beta() -> int:
    return 2

async def gamma() -> int:
    return 3

@gatherable
async def delta() -> int:
    return 4
",
        );
        let names = collect_gatherable_async_fn_names(&module);
        assert!(names.contains("alpha"), "alpha has @gatherable");
        assert!(!names.contains("beta"), "beta is sync");
        assert!(!names.contains("gamma"), "gamma lacks @gatherable");
        assert!(names.contains("delta"));
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn keyword_arg_dependency_breaks_run() {
        let src = "\
@gatherable
async def fa() -> int:
    return 1
@gatherable
async def fb(*, x: int) -> int:
    return x

async def load() -> int:
    a = await fa()
    b = await fb(x=a)
    return a + b
";
        let (_out, stats) = rewrite(src);
        assert_eq!(stats.rewrites, 0);
    }

    #[test]
    fn two_runs_in_one_function_each_get_rewritten() {
        let src = "\
@gatherable
async def fa() -> int:
    return 1
@gatherable
async def fb() -> int:
    return 2
@gatherable
async def fc() -> int:
    return 3
@gatherable
async def fd() -> int:
    return 4

async def load() -> int:
    a = await fa()
    b = await fb()
    print(\"between\")
    c = await fc()
    d = await fd()
    return a + b + c + d
";
        let (_out, stats) = rewrite(src);
        assert_eq!(
            stats.rewrites, 2,
            "two independent runs, two rewrites; got {stats:?}"
        );
        assert_eq!(stats.awaits_folded, 4);
    }

    #[test]
    fn rewrite_emits_flat_block_not_if_true_wrapper() {
        // Regression for the Copilot review: the synthesized rewrite
        // must inline the async-with + result-extracts directly into
        // the enclosing body without an `if True:` no-op block.
        let src = "\
@gatherable
async def fa() -> int:
    return 1
@gatherable
async def fb() -> int:
    return 2

async def load() -> int:
    a = await fa()
    b = await fb()
    return a + b
";
        let (out, _) = rewrite(src);
        assert!(
            !out.contains("if True:"),
            "must not emit `if True:` wrapper; got:\n{out}"
        );
    }

    #[test]
    fn synthesized_names_avoid_existing_collisions() {
        // Regression for the Copilot review: a user binding named
        // `__typhon_autogather_tg_0__` must not collide with the
        // pass's synthesized names. The seeder scans the module and
        // bumps the counter past the highest existing suffix.
        let src = "\
@gatherable
async def fa() -> int:
    return 1
@gatherable
async def fb() -> int:
    return 2

__typhon_autogather_tg_0__ = 99

async def load() -> int:
    a = await fa()
    b = await fb()
    return a + b
";
        let (out, _) = rewrite(src);
        // Counter starts at 1 (past the user's `_0_`).
        assert!(
            out.contains("__typhon_autogather_tg_1__"),
            "expected synthesized name with suffix 1+; got:\n{out}"
        );
        assert!(
            !out.contains("__typhon_autogather_tg_0__ = asyncio.TaskGroup"),
            "must not overwrite the user binding; got:\n{out}"
        );
    }

    // ── detect_missed_gathers (advice diagnostic, no rewrite) ────────────

    #[test]
    fn detect_missed_flags_run_with_missing_decorator() {
        // `fa` is decorated, `fb` is not. The run looks gather-able by
        // shape, so the missed-detection should fire and name `fb`.
        let src = "\
@gatherable
async def fa() -> int:
    return 1

async def fb() -> int:
    return 2

async def load() -> int:
    a = await fa()
    b = await fb()
    return a + b
";
        let module = parse_module(src);
        let missed = detect_missed_gathers(&module);
        assert_eq!(missed.len(), 1, "expected 1 missed run; got {missed:?}");
        assert_eq!(missed[0].missing_callee, "fb");
    }

    #[test]
    fn detect_missed_silent_when_all_decorated() {
        // Both callees are decorated — auto-gather would actually rewrite
        // this run, so no missed-opportunity diagnostic.
        let src = "\
@gatherable
async def fa() -> int:
    return 1

@gatherable
async def fb() -> int:
    return 2

async def load() -> int:
    a = await fa()
    b = await fb()
    return a + b
";
        let module = parse_module(src);
        let missed = detect_missed_gathers(&module);
        assert!(missed.is_empty(), "no missed run expected; got {missed:?}");
    }

    #[test]
    fn detect_missed_silent_when_run_is_dependent() {
        // The run is not gather-able by shape (b depends on a), so
        // there's no missed opportunity even if neither callee is
        // decorated.
        let src = "\
async def fa() -> int:
    return 1

async def fb(x: int) -> int:
    return x

async def load() -> int:
    a = await fa()
    b = await fb(a)
    return a + b
";
        let module = parse_module(src);
        let missed = detect_missed_gathers(&module);
        assert!(missed.is_empty(), "no missed run expected; got {missed:?}");
    }

    #[test]
    fn detect_missed_silent_when_single_await() {
        // A single await is not a gather opportunity in any case.
        let src = "\
async def fa() -> int:
    return 1

async def load() -> int:
    a = await fa()
    return a
";
        let module = parse_module(src);
        let missed = detect_missed_gathers(&module);
        assert!(missed.is_empty(), "single await is never a run; got {missed:?}");
    }

    #[test]
    fn detect_missed_ignores_imported_callees() {
        // The callees here aren't local `async def`s (we never declared
        // them in this module), so detect_missed_gathers must not nudge
        // the user — auto-gather wouldn't touch imported async fns
        // anyway, and the user can't add `@gatherable` to code they
        // don't own.
        let src = "\
import remote

async def load() -> int:
    a = await remote.fetch_a()
    b = await remote.fetch_b()
    return a + b
";
        let module = parse_module(src);
        let missed = detect_missed_gathers(&module);
        assert!(missed.is_empty(), "imported callees ineligible; got {missed:?}");
    }
}
