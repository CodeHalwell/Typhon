//! Free-threading advice lints: `tyc::parallel_opportunity` and
//! `tyc::shared_mut_across_tasks`.
//!
//! Both are **advice** severity — they never block a build — and both only
//! fire when the project targets free-threaded Python
//! (`[python] free-threaded = true`) and the `[strictness] suggest-parallel`
//! knob is on (both the default). The free-threaded gate is what keeps the
//! example / stress corpus (which never sets it) quiet by construction.
//!
//! * `tyc::parallel_opportunity` nudges a comprehension or integer accumulator
//!   loop that *would* be rewritten by `auto-parallel` /
//!   `auto-parallel-reductions` if the knob were on — or a `float` accumulator
//!   that matches every reduction condition except the required `int`
//!   annotation (float addition can only be parallelised by reordering, which
//!   changes the result). It shares the rewrite's exact eligibility predicates
//!   (via [`crate::parallel::detect_parallel_comprehensions`] and
//!   [`crate::reductions::detect_reduction_loops`]).
//!
//! * `tyc::shared_mut_across_tasks` flags a `go`-spawned same-module function
//!   that writes module-level mutable state — a `global` assignment or a write
//!   to a module-level `mut` binding — since under free-threaded Python that
//!   spawned task runs concurrently with the spawner.

use std::collections::{HashMap, HashSet};

use ruff_python_ast::{
    visitor::source_order::{walk_expr, SourceOrderVisitor},
    Expr, ExprCall, ModModule, Mutability, Stmt,
};
use ruff_text_size::{Ranged, TextRange};
use tyc_diagnostics::{Diagnostics, TycError};

/// Emit `tyc::parallel_opportunity` advice for every comprehension /
/// accumulator loop that qualifies for a parallel rewrite whose knob is off,
/// plus every float accumulator loop that would be a reduction but for its
/// `float` annotation.
///
/// `pure_callees` and `min_size` match the rewrite's parameters.
/// `auto_parallel` / `auto_parallel_reductions` are the resolved knob values
/// so the comprehension / int-reduction arms stay silent when the rewrite is
/// already enabled. The caller is responsible for the free-threaded +
/// `suggest-parallel` gate.
pub fn parallel_opportunity_diagnostics(
    module: &ModModule,
    path: &str,
    source: &str,
    pure_callees: &HashSet<String>,
    min_size: u64,
    auto_parallel: bool,
    auto_parallel_reductions: bool,
) -> Diagnostics {
    let mut diags = Diagnostics::new();

    // Comprehensions: rewritten by `auto-parallel` alone. Nudge only when off.
    if !auto_parallel {
        for range in crate::parallel::detect_parallel_comprehensions(module, pure_callees, min_size)
        {
            push_parallel(
                &mut diags,
                "comprehension",
                "set `[strictness] auto-parallel = true` to run the pure element map across \
                 a thread pool on this free-threaded target",
                path,
                source,
                range,
            );
        }
    }

    // Accumulator loops: an `int` reduction is rewritten only when BOTH
    // `auto-parallel` and `auto-parallel-reductions` are on; a `float`
    // reduction is never rewritten (reordering float addition changes the
    // result).
    let reductions_enabled = auto_parallel && auto_parallel_reductions;
    for hit in crate::reductions::detect_reduction_loops(module, pure_callees, min_size) {
        if hit.is_float {
            push_parallel(
                &mut diags,
                "float accumulator loop",
                "float addition is only parallelisable by reordering, which changes the result; \
                 refactor to an `int` accumulation, or use an explicit \
                 `typhon_runtime.parallel.map_pure(...)` + `sum(...)` if the precision tolerance \
                 is acceptable",
                path,
                source,
                hit.range,
            );
        } else if !reductions_enabled {
            let hint = missing_reduction_knobs(auto_parallel, auto_parallel_reductions);
            push_parallel(
                &mut diags,
                "int accumulator loop",
                &hint,
                path,
                source,
                hit.range,
            );
        }
    }

    diags
}

/// Compose the "which knob(s) to flip" hint for an eligible integer reduction.
fn missing_reduction_knobs(auto_parallel: bool, auto_parallel_reductions: bool) -> String {
    match (auto_parallel, auto_parallel_reductions) {
        (false, false) => "set both `[strictness] auto-parallel = true` and \
                            `auto-parallel-reductions = true` to fold this integer accumulation \
                            into a parallel `sum(map_pure(...))`"
            .to_owned(),
        (true, false) => "set `[strictness] auto-parallel-reductions = true` to fold this integer \
                          accumulation into a parallel `sum(map_pure(...))`"
            .to_owned(),
        // reductions on but auto-parallel off (reductions require auto-parallel).
        (false, true) => "set `[strictness] auto-parallel = true` (required by \
                          `auto-parallel-reductions`) to fold this integer accumulation into a \
                          parallel `sum(map_pure(...))`"
            .to_owned(),
        (true, true) => unreachable!("caller only reaches this arm when the rewrite is disabled"),
    }
}

fn push_parallel(
    diags: &mut Diagnostics,
    shape: &str,
    hint: &str,
    path: &str,
    source: &str,
    range: TextRange,
) {
    let offset = range.start().to_usize();
    let length = range.end().to_usize().saturating_sub(offset).max(1);
    diags.push_warning(TycError::parallel_opportunity(
        shape, hint, path, source, offset, length,
    ));
}

/// Emit `tyc::shared_mut_across_tasks` advice for every `go`-spawned
/// same-module function that writes module-level mutable state. The caller
/// applies the free-threaded + `suggest-parallel` gate.
pub fn shared_mut_across_tasks_diagnostics(
    module: &ModModule,
    path: &str,
    source: &str,
) -> Diagnostics {
    let mut diags = Diagnostics::new();

    // Module-level `mut` bindings — the shared state a spawned task must not
    // write unguarded.
    let module_muts: HashSet<&str> = module_level_mut_names(&module.body);

    // Top-level `def NAME` bodies, so a bare-name `go NAME(...)` can be
    // resolved to the function it spawns.
    let mut fn_bodies: HashMap<&str, &[Stmt]> = HashMap::new();
    for stmt in &module.body {
        if let Stmt::FunctionDef(f) = stmt {
            fn_bodies.insert(f.name.as_str(), &f.body);
        }
    }

    // Cache the "writes shared state" verdict per function name.
    let mut verdict: HashMap<String, bool> = HashMap::new();

    for spawn in collect_go_spawns(module) {
        let Some(body) = fn_bodies.get(spawn.callee.as_str()) else {
            continue; // not a bare-name same-module def — stay conservative
        };
        let writes = *verdict
            .entry(spawn.callee.clone())
            .or_insert_with(|| fn_writes_shared_state(body, &module_muts));
        if writes {
            let offset = spawn.range.start().to_usize();
            let length = spawn.range.end().to_usize().saturating_sub(offset).max(1);
            diags.push_warning(TycError::shared_mut_across_tasks(
                spawn.callee.clone(),
                path,
                source,
                offset,
                length,
            ));
        }
    }

    diags
}

/// The names of module-level `mut` bindings (`mut NAME[: T] = …`), including
/// those declared inside module-level control-flow blocks (`if` / `elif` /
/// `else`, `try` / `except` / `finally`, `for` / `while`, `with`, `match`).
/// Function and class bodies are **not** descended into — a `mut` local there is
/// frame state, not module state. Mirrors [`collect_globals`]'s scope rule.
fn module_level_mut_names(body: &[Stmt]) -> HashSet<&str> {
    let mut out = HashSet::new();
    collect_module_mut_names(body, &mut out);
    out
}

fn collect_module_mut_names<'a>(body: &'a [Stmt], out: &mut HashSet<&'a str>) {
    for stmt in body {
        match stmt {
            Stmt::Assign(a) if a.mutability == Some(Mutability::Mut) => {
                for t in &a.targets {
                    if let Expr::Name(n) = t {
                        out.insert(n.id.as_str());
                    }
                }
            }
            Stmt::AnnAssign(a) if a.mutability == Some(Mutability::Mut) => {
                if let Expr::Name(n) = a.target.as_ref() {
                    out.insert(n.id.as_str());
                }
            }
            // Module-level control flow still runs in module scope — descend.
            Stmt::If(s) => {
                collect_module_mut_names(&s.body, out);
                for c in &s.elif_else_clauses {
                    collect_module_mut_names(&c.body, out);
                }
            }
            Stmt::While(s) => {
                collect_module_mut_names(&s.body, out);
                collect_module_mut_names(&s.orelse, out);
            }
            Stmt::For(s) => {
                collect_module_mut_names(&s.body, out);
                collect_module_mut_names(&s.orelse, out);
            }
            Stmt::With(s) => collect_module_mut_names(&s.body, out),
            Stmt::Try(s) => {
                collect_module_mut_names(&s.body, out);
                collect_module_mut_names(&s.orelse, out);
                collect_module_mut_names(&s.finalbody, out);
                for h in &s.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    collect_module_mut_names(&h.body, out);
                }
            }
            Stmt::Match(s) => {
                for case in &s.cases {
                    collect_module_mut_names(&case.body, out);
                }
            }
            // `def` / `class` open their own frame — a `mut` local there is not
            // module state, so don't descend.
            _ => {}
        }
    }
}

/// A discovered `go`-spawn call site.
struct GoSpawn {
    /// The bare-name callee inside `typhon_runtime.tasks.spawn(<callee>(...))`.
    callee: String,
    /// Byte range of the spawn call (for the diagnostic anchor).
    range: TextRange,
}

/// Find every `typhon_runtime.tasks.spawn(CALLEE(...))` in the module (the
/// lowered form of `go CALLEE(...)`), returning the bare-name callee and the
/// spawn call's range. Non-bare-name callees (`go obj.method()`) are skipped.
fn collect_go_spawns(module: &ModModule) -> Vec<GoSpawn> {
    struct V {
        out: Vec<GoSpawn>,
    }
    impl<'ast> SourceOrderVisitor<'ast> for V {
        fn visit_expr(&mut self, e: &'ast Expr) {
            if let Expr::Call(call) = e {
                if is_spawn_call(call) {
                    if let Some(Expr::Call(inner)) = call.arguments.args.first() {
                        if let Expr::Name(callee) = inner.func.as_ref() {
                            self.out.push(GoSpawn {
                                callee: callee.id.to_string(),
                                range: call.range(),
                            });
                        }
                    }
                }
            }
            walk_expr(self, e);
        }
    }
    let mut v = V { out: Vec::new() };
    for stmt in &module.body {
        v.visit_stmt(stmt);
    }
    v.out
}

/// True when `call.func` is the `typhon_runtime.tasks.spawn` attribute chain.
fn is_spawn_call(call: &ExprCall) -> bool {
    let Expr::Attribute(spawn) = call.func.as_ref() else {
        return false;
    };
    if spawn.attr.as_str() != "spawn" {
        return false;
    }
    let Expr::Attribute(tasks) = spawn.value.as_ref() else {
        return false;
    };
    if tasks.attr.as_str() != "tasks" {
        return false;
    }
    matches!(tasks.value.as_ref(), Expr::Name(n) if n.id.as_str() == "typhon_runtime")
}

/// True when the function body writes module-level mutable state: an
/// assignment / augmented-assignment to a name declared `global` in the body,
/// or to a module-level `mut` binding.
fn fn_writes_shared_state(body: &[Stmt], module_muts: &HashSet<&str>) -> bool {
    let mut globals: HashSet<&str> = HashSet::new();
    collect_globals(body, &mut globals);
    body_writes(body, &globals, module_muts)
}

/// Collect every name declared `global` anywhere in the body (recursing into
/// nested blocks, but not into nested `def` / `class`, which have their own
/// global scope).
fn collect_globals<'a>(body: &'a [Stmt], out: &mut HashSet<&'a str>) {
    for stmt in body {
        match stmt {
            Stmt::Global(g) => {
                for n in &g.names {
                    out.insert(n.as_str());
                }
            }
            Stmt::If(s) => {
                collect_globals(&s.body, out);
                for c in &s.elif_else_clauses {
                    collect_globals(&c.body, out);
                }
            }
            Stmt::While(s) => {
                collect_globals(&s.body, out);
                collect_globals(&s.orelse, out);
            }
            Stmt::For(s) => {
                collect_globals(&s.body, out);
                collect_globals(&s.orelse, out);
            }
            Stmt::With(s) => collect_globals(&s.body, out),
            Stmt::Try(s) => {
                collect_globals(&s.body, out);
                collect_globals(&s.orelse, out);
                collect_globals(&s.finalbody, out);
                for h in &s.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    collect_globals(&h.body, out);
                }
            }
            Stmt::Match(s) => {
                for case in &s.cases {
                    collect_globals(&case.body, out);
                }
            }
            _ => {}
        }
    }
}

/// True when any assignment / augmented-assignment in `body` targets a name in
/// `globals` or `module_muts`. Recurses into nested blocks but not into nested
/// `def` / `class` (their writes belong to a different frame).
fn body_writes(body: &[Stmt], globals: &HashSet<&str>, module_muts: &HashSet<&str>) -> bool {
    let hits = |name: &str| globals.contains(name) || module_muts.contains(name);
    for stmt in body {
        match stmt {
            Stmt::Assign(a) => {
                if a.targets.iter().any(|t| target_hits(t, &hits)) {
                    return true;
                }
            }
            Stmt::AnnAssign(a) => {
                if a.value.is_some() && target_hits(&a.target, &hits) {
                    return true;
                }
            }
            Stmt::AugAssign(a) => {
                if target_hits(&a.target, &hits) {
                    return true;
                }
            }
            Stmt::If(s) => {
                if body_writes(&s.body, globals, module_muts)
                    || s.elif_else_clauses
                        .iter()
                        .any(|c| body_writes(&c.body, globals, module_muts))
                {
                    return true;
                }
            }
            Stmt::While(s) => {
                if body_writes(&s.body, globals, module_muts)
                    || body_writes(&s.orelse, globals, module_muts)
                {
                    return true;
                }
            }
            Stmt::For(s) => {
                if body_writes(&s.body, globals, module_muts)
                    || body_writes(&s.orelse, globals, module_muts)
                {
                    return true;
                }
            }
            Stmt::With(s) => {
                if body_writes(&s.body, globals, module_muts) {
                    return true;
                }
            }
            Stmt::Try(s) => {
                if body_writes(&s.body, globals, module_muts)
                    || body_writes(&s.orelse, globals, module_muts)
                    || body_writes(&s.finalbody, globals, module_muts)
                    || s.handlers.iter().any(|h| {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                        body_writes(&h.body, globals, module_muts)
                    })
                {
                    return true;
                }
            }
            Stmt::Match(s) => {
                if s.cases
                    .iter()
                    .any(|case| body_writes(&case.body, globals, module_muts))
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// True when an assignment target (a bare name, or a tuple/list unpack) hits a
/// shared name per `hits`.
fn target_hits(target: &Expr, hits: &impl Fn(&str) -> bool) -> bool {
    match target {
        Expr::Name(n) => hits(n.id.as_str()),
        Expr::Tuple(t) => t.elts.iter().any(|e| target_hits(e, hits)),
        Expr::List(l) => l.elts.iter().any(|e| target_hits(e, hits)),
        Expr::Starred(s) => target_hits(&s.value, hits),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miette::Diagnostic as _;

    fn parse(src: &str) -> ModModule {
        // Mirror the real pipeline: `go` (and the other surface expansions)
        // are applied before `preprocess`, so the parsed module sees the
        // lowered `typhon_runtime.tasks.spawn(...)` the shared-mut lint keys on.
        let expanded = tyc_syntax::preprocess::expand_go_calls(src);
        let prep = tyc_syntax::preprocess::preprocess(&expanded);
        tyc_syntax::parse_module(&prep.python_source)
            .expect("parse failed")
            .into_syntax()
    }

    fn pure_set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    fn codes(diags: &Diagnostics) -> Vec<String> {
        diags
            .warnings()
            .iter()
            .filter_map(|w| w.code().map(|c| c.to_string()))
            .collect()
    }

    // ── parallel_opportunity ──────────────────────────────────────────────

    #[test]
    fn parallel_opportunity_flags_comprehension_when_off() {
        let src = "ys: list[int] = [f(x) for x in xs]\n";
        let m = parse(src);
        let diags =
            parallel_opportunity_diagnostics(&m, "x.ty", src, &pure_set(&["f"]), 0, false, false);
        assert_eq!(codes(&diags).len(), 1);
        assert!(codes(&diags)[0].contains("parallel_opportunity"));
    }

    #[test]
    fn parallel_opportunity_silent_for_comprehension_when_on() {
        let src = "ys: list[int] = [f(x) for x in xs]\n";
        let m = parse(src);
        // auto_parallel on → the comprehension would already be rewritten.
        let diags =
            parallel_opportunity_diagnostics(&m, "x.ty", src, &pure_set(&["f"]), 0, true, false);
        assert_eq!(codes(&diags).len(), 0);
    }

    #[test]
    fn parallel_opportunity_flags_int_reduction_when_off() {
        let src = "\
def run(xs: list[int]) -> int:
    mut total: int = 0
    for x in xs:
        total += x
    return total
";
        let m = parse(src);
        let diags =
            parallel_opportunity_diagnostics(&m, "x.ty", src, &pure_set(&[]), 0, true, false);
        let c = codes(&diags);
        assert_eq!(c.len(), 1, "int reduction with reductions off should fire");
        assert!(c[0].contains("parallel_opportunity"));
    }

    #[test]
    fn parallel_opportunity_silent_for_int_reduction_when_both_on() {
        let src = "\
def run(xs: list[int]) -> int:
    mut total: int = 0
    for x in xs:
        total += x
    return total
";
        let m = parse(src);
        let diags =
            parallel_opportunity_diagnostics(&m, "x.ty", src, &pure_set(&[]), 0, true, true);
        assert_eq!(codes(&diags).len(), 0);
    }

    #[test]
    fn parallel_opportunity_flags_float_reduction_regardless_of_knobs() {
        let src = "\
def run(xs: list[float]) -> float:
    mut total: float = 0.0
    for x in xs:
        total += x
    return total
";
        let m = parse(src);
        // Even with both knobs on, a float reduction is never auto-rewritten.
        let diags =
            parallel_opportunity_diagnostics(&m, "x.ty", src, &pure_set(&[]), 0, true, true);
        assert_eq!(codes(&diags).len(), 1, "float reduction always flagged");
    }

    // ── shared_mut_across_tasks ───────────────────────────────────────────

    #[test]
    fn shared_mut_flags_go_callee_writing_global() {
        let src = "\
mut counter: int = 0

async def worker() -> None:
    global counter
    counter = counter + 1

async def main() -> None:
    go worker()
";
        let m = parse(src);
        let diags = shared_mut_across_tasks_diagnostics(&m, "x.ty", src);
        let c = codes(&diags);
        assert_eq!(c.len(), 1, "go-spawned global writer should be flagged");
        assert!(c[0].contains("shared_mut_across_tasks"));
    }

    #[test]
    fn shared_mut_flags_go_callee_writing_module_mut() {
        let src = "\
mut hits: int = 0

async def worker() -> None:
    hits += 1

async def main() -> None:
    go worker() -> task
";
        let m = parse(src);
        let diags = shared_mut_across_tasks_diagnostics(&m, "x.ty", src);
        assert_eq!(
            codes(&diags).len(),
            1,
            "aug-assign to module mut should fire"
        );
    }

    #[test]
    fn shared_mut_silent_for_pure_callee() {
        let src = "\
async def worker() -> int:
    let x: int = 1
    return x

async def main() -> None:
    go worker()
";
        let m = parse(src);
        assert_eq!(
            codes(&shared_mut_across_tasks_diagnostics(&m, "x.ty", src)).len(),
            0,
            "a callee with no shared write must be silent"
        );
    }

    #[test]
    fn shared_mut_silent_for_method_callee() {
        // `go obj.method()` — not a bare-name same-module def, so skipped.
        let src = "\
mut counter: int = 0

async def main(obj) -> None:
    go obj.tick()
";
        let m = parse(src);
        assert_eq!(
            codes(&shared_mut_across_tasks_diagnostics(&m, "x.ty", src)).len(),
            0
        );
    }

    #[test]
    fn shared_mut_flags_module_mut_declared_inside_if() {
        // Finding 5: a module-level `mut` declared inside a module-level `if`
        // block is still module state. `module_level_mut_names` used to scan
        // only top-level statements, so a go-spawned writer under-warned.
        let src = "\
if True:
    mut hits: int = 0

async def worker() -> None:
    hits += 1

async def main() -> None:
    go worker()
";
        let m = parse(src);
        let c = codes(&shared_mut_across_tasks_diagnostics(&m, "x.ty", src));
        assert_eq!(
            c.len(),
            1,
            "a module `mut` inside a top-level `if` is shared state"
        );
        assert!(c[0].contains("shared_mut_across_tasks"));
    }

    #[test]
    fn shared_mut_silent_for_mut_inside_function_body() {
        // The scope rule must stay one-sided: a `mut` local inside a *function*
        // body is frame state, not module state, so writing a same-named name
        // from a go-spawned callee is not a shared-state race.
        let src = "\
def setup() -> None:
    mut hits: int = 0

async def worker() -> None:
    hits += 1

async def main() -> None:
    go worker()
";
        let m = parse(src);
        assert_eq!(
            codes(&shared_mut_across_tasks_diagnostics(&m, "x.ty", src)).len(),
            0,
            "a function-local `mut` is not module state"
        );
    }
}
