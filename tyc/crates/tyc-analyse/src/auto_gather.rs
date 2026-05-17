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

use rustpython_ast::{
    text_size::TextRange, Expr, ExprAttribute, ExprCall, ExprContext, ExprName, Identifier, Mod,
    ModModule, Stmt, StmtAssign, StmtAsyncWith, WithItem,
};

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
/// proven safe to gather (typically: all module-level `async def`s in
/// the current file).
pub fn rewrite_auto_gather(
    module: Mod<TextRange>,
    eligible_async_fns: &HashSet<String>,
) -> (Mod<TextRange>, AutoGatherStats) {
    let mut stats = AutoGatherStats::default();
    let mut counter: usize = 0;
    let module = match module {
        Mod::Module(m) => {
            let body = rewrite_stmts(
                m.body,
                eligible_async_fns,
                /* inside_async */ false,
                &mut counter,
                &mut stats,
            );
            Mod::Module(ModModule {
                range: m.range,
                body,
                type_ignores: m.type_ignores,
            })
        }
        other => other,
    };
    (module, stats)
}

// ── walker ───────────────────────────────────────────────────────────────────

fn rewrite_stmts(
    body: Vec<Stmt<TextRange>>,
    eligible: &HashSet<String>,
    inside_async: bool,
    counter: &mut usize,
    stats: &mut AutoGatherStats,
) -> Vec<Stmt<TextRange>> {
    let mut out: Vec<Stmt<TextRange>> = Vec::with_capacity(body.len());
    // Move into an iterator so we can take ownership of each stmt without
    // cloning. We need lookahead for run-detection, so peek at the upcoming
    // slice via an index into the source `Vec`.
    let mut i = 0;
    while i < body.len() {
        if inside_async {
            let run = collect_run(&body, i, eligible);
            if run.len() >= 2 {
                let block = make_autogather_block(&run, counter);
                out.push(block);
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
    stmt: Stmt<TextRange>,
    eligible: &HashSet<String>,
    inside_async: bool,
    counter: &mut usize,
    stats: &mut AutoGatherStats,
) -> Stmt<TextRange> {
    match stmt {
        Stmt::FunctionDef(mut f) => {
            f.body = rewrite_stmts(f.body, eligible, false, counter, stats);
            Stmt::FunctionDef(f)
        }
        Stmt::AsyncFunctionDef(mut f) => {
            f.body = rewrite_stmts(f.body, eligible, true, counter, stats);
            Stmt::AsyncFunctionDef(f)
        }
        Stmt::ClassDef(mut c) => {
            // Methods inside a class body run their own walk; whether they're
            // async is determined per-method.
            c.body = rewrite_stmts(c.body, eligible, false, counter, stats);
            Stmt::ClassDef(c)
        }
        Stmt::If(mut s) => {
            s.body = rewrite_stmts(s.body, eligible, inside_async, counter, stats);
            s.orelse = rewrite_stmts(s.orelse, eligible, inside_async, counter, stats);
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
        Stmt::AsyncFor(mut s) => {
            s.body = rewrite_stmts(s.body, eligible, inside_async, counter, stats);
            s.orelse = rewrite_stmts(s.orelse, eligible, inside_async, counter, stats);
            Stmt::AsyncFor(s)
        }
        Stmt::With(mut s) => {
            s.body = rewrite_stmts(s.body, eligible, inside_async, counter, stats);
            Stmt::With(s)
        }
        Stmt::AsyncWith(mut s) => {
            s.body = rewrite_stmts(s.body, eligible, inside_async, counter, stats);
            Stmt::AsyncWith(s)
        }
        Stmt::Try(mut s) => {
            s.body = rewrite_stmts(s.body, eligible, inside_async, counter, stats);
            for h in s.handlers.iter_mut() {
                let rustpython_ast::ExceptHandler::ExceptHandler(h) = h;
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
        Stmt::TryStar(mut s) => {
            s.body = rewrite_stmts(s.body, eligible, inside_async, counter, stats);
            for h in s.handlers.iter_mut() {
                let rustpython_ast::ExceptHandler::ExceptHandler(h) = h;
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
            Stmt::TryStar(s)
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
    args: Vec<Expr<TextRange>>,
    keywords: Vec<rustpython_ast::Keyword<TextRange>>,
    call_range: TextRange,
}

/// Scan `body[start..]` and collect the longest prefix that forms a safe
/// gather run.
fn collect_run(
    body: &[Stmt<TextRange>],
    start: usize,
    eligible: &HashSet<String>,
) -> Vec<Candidate> {
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
fn parse_candidate(stmt: &Stmt<TextRange>, eligible: &HashSet<String>) -> Option<Candidate> {
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
        args: call.args.clone(),
        keywords: call.keywords.clone(),
        call_range: call.range,
    })
}

/// `true` if any expression in the call references a name in `bound`.
fn call_uses_any(
    args: &[Expr<TextRange>],
    keywords: &[rustpython_ast::Keyword<TextRange>],
    bound: &HashSet<String>,
) -> bool {
    args.iter().any(|e| expr_uses_any(e, bound))
        || keywords.iter().any(|k| expr_uses_any(&k.value, bound))
}

fn expr_uses_any(expr: &Expr<TextRange>, bound: &HashSet<String>) -> bool {
    match expr {
        Expr::Name(n) => bound.contains(n.id.as_str()),
        Expr::Call(c) => {
            expr_uses_any(&c.func, bound)
                || c.args.iter().any(|e| expr_uses_any(e, bound))
                || c.keywords.iter().any(|k| expr_uses_any(&k.value, bound))
        }
        Expr::Attribute(a) => expr_uses_any(&a.value, bound),
        Expr::Subscript(s) => expr_uses_any(&s.value, bound) || expr_uses_any(&s.slice, bound),
        Expr::BinOp(b) => expr_uses_any(&b.left, bound) || expr_uses_any(&b.right, bound),
        Expr::UnaryOp(u) => expr_uses_any(&u.operand, bound),
        Expr::BoolOp(b) => b.values.iter().any(|e| expr_uses_any(e, bound)),
        Expr::Compare(c) => {
            expr_uses_any(&c.left, bound) || c.comparators.iter().any(|e| expr_uses_any(e, bound))
        }
        Expr::IfExp(i) => {
            expr_uses_any(&i.test, bound)
                || expr_uses_any(&i.body, bound)
                || expr_uses_any(&i.orelse, bound)
        }
        Expr::Lambda(_) => true, // conservative: don't peek into lambda bodies
        Expr::Tuple(t) => t.elts.iter().any(|e| expr_uses_any(e, bound)),
        Expr::List(l) => l.elts.iter().any(|e| expr_uses_any(e, bound)),
        Expr::Set(s) => s.elts.iter().any(|e| expr_uses_any(e, bound)),
        Expr::Dict(d) => {
            d.values.iter().any(|e| expr_uses_any(e, bound))
                || d.keys.iter().flatten().any(|e| expr_uses_any(e, bound))
        }
        Expr::Starred(s) => expr_uses_any(&s.value, bound),
        Expr::Await(a) => expr_uses_any(&a.value, bound),
        Expr::FormattedValue(f) => expr_uses_any(&f.value, bound),
        Expr::JoinedStr(j) => j.values.iter().any(|e| expr_uses_any(e, bound)),
        // Generators, comprehensions: skip; they introduce their own scopes.
        // Constants, ellipsis: never reference bound names.
        _ => false,
    }
}

// ── synthesis ────────────────────────────────────────────────────────────────

fn make_autogather_block(run: &[Candidate], counter: &mut usize) -> Stmt<TextRange> {
    let id = *counter;
    *counter += 1;

    let tg_name = format!("__typhon_autogather_tg_{}__", id);
    let task_name = |i: usize| format!("__typhon_autogather_task_{}_{}__", id, i);

    // Body of the `async with` block: one `task_i = tg.create_task(callee(args))`
    // per candidate.
    let mut block_body: Vec<Stmt<TextRange>> = Vec::with_capacity(run.len());
    for (i, c) in run.iter().enumerate() {
        block_body.push(make_create_task_assign(&tg_name, &task_name(i), c));
    }

    // The async-with statement itself.
    let async_with = Stmt::AsyncWith(StmtAsyncWith {
        range: TextRange::default(),
        items: vec![WithItem {
            range: TextRange::default().into(),
            context_expr: make_taskgroup_call(),
            optional_vars: Some(Box::new(name_store(&tg_name))),
        }],
        body: block_body,
        type_comment: None,
    });

    // Result-extraction assignments AFTER the async-with block. To return them
    // as a single Stmt we wrap the whole sequence in an `if True:` block — the
    // emitter prints these as a flat block at the same indent as the surrounding
    // body, which is the same trick `unsafe:` uses elsewhere in the pipeline.
    let mut grouped: Vec<Stmt<TextRange>> = Vec::with_capacity(1 + run.len());
    grouped.push(async_with);
    for (i, c) in run.iter().enumerate() {
        grouped.push(make_result_extract(&c.bind, &task_name(i)));
    }
    Stmt::If(rustpython_ast::StmtIf {
        range: TextRange::default(),
        test: Box::new(true_const()),
        body: grouped,
        orelse: vec![],
    })
}

fn name_load(name: &str) -> Expr<TextRange> {
    Expr::Name(ExprName {
        range: TextRange::default(),
        id: Identifier::new(name),
        ctx: ExprContext::Load,
    })
}

fn name_store(name: &str) -> Expr<TextRange> {
    Expr::Name(ExprName {
        range: TextRange::default(),
        id: Identifier::new(name),
        ctx: ExprContext::Store,
    })
}

fn true_const() -> Expr<TextRange> {
    Expr::Constant(rustpython_ast::ExprConstant {
        range: TextRange::default(),
        value: rustpython_ast::Constant::Bool(true),
        kind: None,
    })
}

fn make_taskgroup_call() -> Expr<TextRange> {
    // asyncio.TaskGroup()
    Expr::Call(ExprCall {
        range: TextRange::default(),
        func: Box::new(Expr::Attribute(ExprAttribute {
            range: TextRange::default(),
            value: Box::new(name_load("asyncio")),
            attr: Identifier::new("TaskGroup"),
            ctx: ExprContext::Load,
        })),
        args: vec![],
        keywords: vec![],
    })
}

fn make_create_task_assign(tg: &str, task: &str, c: &Candidate) -> Stmt<TextRange> {
    // task = tg.create_task(callee(args))
    let inner_call = Expr::Call(ExprCall {
        range: c.call_range,
        func: Box::new(name_load(&c.call_func)),
        args: c.args.clone(),
        keywords: c.keywords.clone(),
    });
    let create_task = Expr::Call(ExprCall {
        range: TextRange::default(),
        func: Box::new(Expr::Attribute(ExprAttribute {
            range: TextRange::default(),
            value: Box::new(name_load(tg)),
            attr: Identifier::new("create_task"),
            ctx: ExprContext::Load,
        })),
        args: vec![inner_call],
        keywords: vec![],
    });
    Stmt::Assign(StmtAssign {
        range: TextRange::default(),
        targets: vec![name_store(task)],
        value: Box::new(create_task),
        type_comment: None,
    })
}

fn make_result_extract(bind: &str, task: &str) -> Stmt<TextRange> {
    // bind = task.result()
    let result_call = Expr::Call(ExprCall {
        range: TextRange::default(),
        func: Box::new(Expr::Attribute(ExprAttribute {
            range: TextRange::default(),
            value: Box::new(name_load(task)),
            attr: Identifier::new("result"),
            ctx: ExprContext::Load,
        })),
        args: vec![],
        keywords: vec![],
    });
    Stmt::Assign(StmtAssign {
        range: TextRange::default(),
        targets: vec![name_store(bind)],
        value: Box::new(result_call),
        type_comment: None,
    })
}

// ── module-level fn collection ──────────────────────────────────────────────

/// Convenience: collect the names of every top-level `async def` in `module`.
/// The build pipeline uses this as the default `eligible_async_fns` set when
/// `auto_gather = true`.
pub fn collect_module_async_fn_names(module: &Mod<TextRange>) -> HashSet<String> {
    let mut out = HashSet::new();
    if let Mod::Module(m) = module {
        for stmt in &m.body {
            if let Stmt::AsyncFunctionDef(f) = stmt {
                out.insert(f.name.as_str().to_owned());
            }
        }
    }
    out
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rustpython_parser::{parse, Mode};

    fn parse_module(src: &str) -> Mod<TextRange> {
        parse(src, Mode::Module, "<test>").expect("parse failed")
    }

    fn render(module: &Mod<TextRange>) -> String {
        tyc_emit::emit(module)
    }

    fn rewrite(src: &str) -> (String, AutoGatherStats) {
        let module = parse_module(src);
        let eligible = collect_module_async_fn_names(&module);
        let (rewritten, stats) = rewrite_auto_gather(module, &eligible);
        (render(&rewritten), stats)
    }

    #[test]
    fn two_independent_awaits_are_folded() {
        let src = "\
async def fetch_a() -> int:
    return 1

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
async def fetch_a() -> int:
    return 1

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
    fn single_await_not_rewritten() {
        let src = "\
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
async def fetch_a() -> int:
    return 1

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
        // `external_fetch` isn't in the module's async fn set, so the
        // walker treats it as opaque and doesn't gather.
        let src = "\
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
    fn three_independent_awaits_fold_together() {
        let src = "\
async def fa() -> int:
    return 1
async def fb() -> int:
    return 2
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
async def fa() -> int:
    return 1
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
async def fa() -> int:
    return 1
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
    fn collect_module_async_fn_names_finds_top_level_async_defs() {
        let module = parse_module(
            "\
async def alpha() -> int:
    return 1

def beta() -> int:
    return 2

async def gamma() -> int:
    return 3
",
        );
        let names = collect_module_async_fn_names(&module);
        assert!(names.contains("alpha"));
        assert!(!names.contains("beta"));
        assert!(names.contains("gamma"));
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn keyword_arg_dependency_breaks_run() {
        let src = "\
async def fa() -> int:
    return 1
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
async def fa() -> int:
    return 1
async def fb() -> int:
    return 2
async def fc() -> int:
    return 3
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
}
