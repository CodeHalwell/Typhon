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
//!   that writes module-level mutable state — a `global` assignment, a write
//!   to a module-level `mut` binding, an in-place mutation of module-level
//!   state (`SEEN[k] = …`, `LOG.append(…)`, `del CACHE[k]`, `Cls.attr = …`),
//!   or a call to a same-module helper that does any of these — since under
//!   free-threaded Python that spawned task runs concurrently with the
//!   spawner.

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
    let module_muts = module_level_mut_names(&module.body);

    // Top-level `def NAME` bodies, so a bare-name `go NAME(...)` can be
    // resolved to the function it spawns.
    let mut fn_bodies: HashMap<&str, &[Stmt]> = HashMap::new();
    for stmt in &module.body {
        if let Stmt::FunctionDef(f) = stmt {
            fn_bodies.insert(f.name.as_str(), &f.body);
        }
    }

    // Cache the "writes shared state" verdict per function name.
    let ctx = SharedWriteCtx {
        module_names: module_level_names(&module.body),
        module_muts: &module_muts,
        fns: module
            .body
            .iter()
            .filter_map(|stmt| match stmt {
                Stmt::FunctionDef(f) => Some((f.name.as_str(), f)),
                _ => None,
            })
            .collect(),
        cache: std::cell::RefCell::new(HashMap::new()),
    };

    for spawn in collect_go_spawns(module) {
        if !fn_bodies.contains_key(spawn.callee) {
            continue; // not a bare-name same-module def — stay conservative
        }
        if ctx.writes(spawn.callee, &mut HashSet::new()) {
            let offset = spawn.range.start().to_usize();
            let length = spawn.range.end().to_usize().saturating_sub(offset).max(1);
            diags.push_warning(TycError::shared_mut_across_tasks(
                spawn.callee.to_owned(),
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
struct GoSpawn<'a> {
    /// The bare-name callee inside `typhon_runtime.tasks.spawn(<callee>(...))`.
    callee: &'a str,
    /// Byte range of the spawn call (for the diagnostic anchor).
    range: TextRange,
}

/// Find every `typhon_runtime.tasks.spawn(CALLEE(...))` in the module (the
/// lowered form of `go CALLEE(...)`), returning the bare-name callee and the
/// spawn call's range. Non-bare-name callees (`go obj.method()`) are skipped,
/// and so is a callee an enclosing function binds (`async def
/// launch(worker): go worker()` spawns the argument, not the module-level
/// `worker`) — the per-scope shadow rule auto-gather uses.
fn collect_go_spawns<'a>(module: &'a ModModule) -> Vec<GoSpawn<'a>> {
    use ruff_python_ast::visitor::source_order::walk_stmt;

    struct V<'a> {
        out: Vec<GoSpawn<'a>>,
        /// Enclosing functions, innermost last. Class bodies are left out:
        /// their bindings are not visible from the methods inside them.
        scopes: Vec<&'a ruff_python_ast::StmtFunctionDef>,
    }
    impl V<'_> {
        fn shadowed(&self, name: &str) -> bool {
            self.scopes.iter().any(|f| {
                crate::reductions::params_bind_name(&f.parameters, name)
                    || crate::reductions::scope_binds_name(&f.body, name)
            })
        }
    }
    impl<'ast> SourceOrderVisitor<'ast> for V<'ast> {
        fn visit_stmt(&mut self, s: &'ast Stmt) {
            if let Stmt::FunctionDef(f) = s {
                self.scopes.push(f);
                walk_stmt(self, s);
                self.scopes.pop();
            } else {
                walk_stmt(self, s);
            }
        }

        fn visit_expr(&mut self, e: &'ast Expr) {
            if let Expr::Call(call) = e {
                if is_spawn_call(call) {
                    if let Some(Expr::Call(inner)) = call.arguments.args.first() {
                        if let Expr::Name(callee) = inner.func.as_ref() {
                            if !self.shadowed(callee.id.as_str()) {
                                self.out.push(GoSpawn {
                                    callee: callee.id.as_str(),
                                    range: call.range(),
                                });
                            }
                        }
                    }
                }
            }
            walk_expr(self, e);
        }
    }
    let mut v = V {
        out: Vec::new(),
        scopes: Vec::new(),
    };
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

/// Methods that mutate their receiver in place (`list`, `dict`, `set`,
/// `deque`, `bytearray`): calling one on module-level state from a spawned
/// task is a shared write.
const MUTATING_METHODS: &[&str] = &[
    "append",
    "extend",
    "insert",
    "pop",
    "remove",
    "clear",
    "update",
    "add",
    "discard",
    "setdefault",
    "popitem",
    "sort",
    "reverse",
    "appendleft",
    "extendleft",
    "popleft",
    "rotate",
    "difference_update",
    "intersection_update",
    "symmetric_difference_update",
    "__setitem__",
    "__delitem__",
];

/// What a function may write on a spawned task's behalf.
struct SharedWriteCtx<'a, 'm> {
    /// Every name bound at module level (`let`, `mut`, plain assignment,
    /// classes): mutating one in place is a shared write.
    module_names: HashSet<&'a str>,
    /// Module-level `mut` bindings: rebinding one is a shared write.
    module_muts: &'m HashSet<&'a str>,
    fns: HashMap<&'a str, &'a ruff_python_ast::StmtFunctionDef>,
    cache: std::cell::RefCell<HashMap<&'a str, bool>>,
}

impl<'a> SharedWriteCtx<'a, '_> {
    /// True when the same-module function `name` writes module-level state —
    /// rebinds a `global` / module `mut`, mutates module-level state in place
    /// (`SEEN[k] = …`, `LOG.append(…)`, `Cls.attr = …`, `del CACHE[k]`) — or
    /// calls a same-module helper that does.
    fn writes(&self, name: &'a str, visiting: &mut HashSet<&'a str>) -> bool {
        if let Some(&known) = self.cache.borrow().get(name) {
            return known;
        }
        let Some(f) = self.fns.get(name) else {
            return false;
        };
        if !visiting.insert(name) {
            return false;
        }
        let mut globals: HashSet<&'a str> = HashSet::new();
        collect_globals(&f.body, &mut globals);
        let mut locals: HashSet<&'a str> = HashSet::new();
        for p in f.parameters.iter() {
            locals.insert(p.name().as_str());
        }
        collect_local_bindings(&f.body, &mut locals);
        for g in &globals {
            locals.remove(g);
        }
        let mut finder = WriteFinder {
            globals: &globals,
            locals: &locals,
            ctx: self,
            found: false,
            helpers: Vec::new(),
        };
        for stmt in &f.body {
            finder.visit_stmt(stmt);
        }
        let writes = finder.found
            || finder
                .helpers
                .clone()
                .into_iter()
                .any(|h| self.writes(h, visiting));
        visiting.remove(name);
        self.cache.borrow_mut().insert(name, writes);
        writes
    }
}

struct WriteFinder<'a, 'c, 'm> {
    globals: &'c HashSet<&'a str>,
    locals: &'c HashSet<&'a str>,
    ctx: &'c SharedWriteCtx<'a, 'm>,
    found: bool,
    helpers: Vec<&'a str>,
}

impl WriteFinder<'_, '_, '_> {
    fn rebinds_shared(&self, name: &str) -> bool {
        self.globals.contains(name) || self.ctx.module_muts.contains(name)
    }

    fn is_shared_root(&self, name: &str) -> bool {
        !self.locals.contains(name)
            && (self.globals.contains(name) || self.ctx.module_names.contains(name))
    }

    /// A store into module-level state: a rebinding of a shared name, or an
    /// item / attribute store whose root is one.
    fn target_writes(&self, target: &Expr) -> bool {
        match target {
            Expr::Name(n) => self.rebinds_shared(n.id.as_str()),
            Expr::Subscript(_) | Expr::Attribute(_) => {
                root_name(target).is_some_and(|r| self.is_shared_root(r))
            }
            Expr::Tuple(t) => t.elts.iter().any(|e| self.target_writes(e)),
            Expr::List(l) => l.elts.iter().any(|e| self.target_writes(e)),
            Expr::Starred(s) => self.target_writes(&s.value),
            _ => false,
        }
    }
}

impl<'a> SourceOrderVisitor<'a> for WriteFinder<'a, '_, '_> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            // A nested `def` / `class` is another frame.
            Stmt::FunctionDef(_) | Stmt::ClassDef(_) => return,
            Stmt::Assign(a) => {
                if a.targets.iter().any(|t| self.target_writes(t)) {
                    self.found = true;
                }
            }
            Stmt::AnnAssign(a) => {
                if a.value.is_some() && self.target_writes(&a.target) {
                    self.found = true;
                }
            }
            Stmt::AugAssign(a) => {
                if self.target_writes(&a.target) {
                    self.found = true;
                }
            }
            Stmt::Delete(d) => {
                if d.targets.iter().any(|t| {
                    matches!(t, Expr::Subscript(_) | Expr::Attribute(_)) && self.target_writes(t)
                }) {
                    self.found = true;
                }
            }
            _ => {}
        }
        ruff_python_ast::visitor::source_order::walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, e: &'a Expr) {
        if let Expr::Call(call) = e {
            match call.func.as_ref() {
                Expr::Attribute(attr)
                    if MUTATING_METHODS.contains(&attr.attr.as_str())
                        && root_name(&attr.value).is_some_and(|r| self.is_shared_root(r)) =>
                {
                    self.found = true;
                }
                Expr::Name(n)
                    if !self.locals.contains(n.id.as_str())
                        && self.ctx.fns.contains_key(n.id.as_str()) =>
                {
                    self.helpers.push(n.id.as_str());
                }
                _ => {}
            }
        }
        walk_expr(self, e);
    }
}

/// The name an item / attribute chain is rooted at (`SEEN` in
/// `SEEN[k].x`).
fn root_name(e: &Expr) -> Option<&str> {
    match e {
        Expr::Name(n) => Some(n.id.as_str()),
        Expr::Subscript(s) => root_name(&s.value),
        Expr::Attribute(a) => root_name(&a.value),
        _ => None,
    }
}

/// Every name bound at module level, through module-level control flow
/// (`let`, `mut`, plain and annotated assignment, `class`).
fn module_level_names(body: &[Stmt]) -> HashSet<&str> {
    fn walk<'a>(body: &'a [Stmt], out: &mut HashSet<&'a str>) {
        for stmt in body {
            match stmt {
                Stmt::Assign(a) => {
                    for t in &a.targets {
                        if let Expr::Name(n) = t {
                            out.insert(n.id.as_str());
                        }
                    }
                }
                Stmt::AnnAssign(a) => {
                    if let Expr::Name(n) = a.target.as_ref() {
                        out.insert(n.id.as_str());
                    }
                }
                Stmt::ClassDef(c) => {
                    out.insert(c.name.as_str());
                }
                Stmt::If(s) => {
                    walk(&s.body, out);
                    for c in &s.elif_else_clauses {
                        walk(&c.body, out);
                    }
                }
                Stmt::While(s) => {
                    walk(&s.body, out);
                    walk(&s.orelse, out);
                }
                Stmt::For(s) => {
                    walk(&s.body, out);
                    walk(&s.orelse, out);
                }
                Stmt::With(s) => walk(&s.body, out),
                Stmt::Try(s) => {
                    walk(&s.body, out);
                    walk(&s.orelse, out);
                    walk(&s.finalbody, out);
                    for h in &s.handlers {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                        walk(&h.body, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = HashSet::new();
    walk(body, &mut out);
    out
}

/// Names a function body binds locally (assignment, `for` / `with`
/// targets, `except … as`), not descending into nested `def` / `class`.
fn collect_local_bindings<'a>(body: &'a [Stmt], out: &mut HashSet<&'a str>) {
    fn names<'a>(t: &'a Expr, out: &mut HashSet<&'a str>) {
        match t {
            Expr::Name(n) => {
                out.insert(n.id.as_str());
            }
            Expr::Tuple(t) => t.elts.iter().for_each(|e| names(e, out)),
            Expr::List(l) => l.elts.iter().for_each(|e| names(e, out)),
            Expr::Starred(s) => names(&s.value, out),
            _ => {}
        }
    }
    for stmt in body {
        match stmt {
            Stmt::Assign(a) => a.targets.iter().for_each(|t| names(t, out)),
            Stmt::AnnAssign(a) => names(&a.target, out),
            Stmt::AugAssign(a) => names(&a.target, out),
            Stmt::FunctionDef(f) => {
                out.insert(f.name.as_str());
            }
            Stmt::ClassDef(c) => {
                out.insert(c.name.as_str());
            }
            Stmt::Import(i) => {
                for a in &i.names {
                    let local = a.asname.as_ref().unwrap_or(&a.name);
                    out.insert(local.as_str().split('.').next().unwrap_or(""));
                }
            }
            Stmt::ImportFrom(i) => {
                for a in &i.names {
                    out.insert(a.asname.as_ref().unwrap_or(&a.name).as_str());
                }
            }
            Stmt::If(s) => {
                collect_local_bindings(&s.body, out);
                for c in &s.elif_else_clauses {
                    collect_local_bindings(&c.body, out);
                }
            }
            Stmt::While(s) => {
                collect_local_bindings(&s.body, out);
                collect_local_bindings(&s.orelse, out);
            }
            Stmt::For(s) => {
                names(&s.target, out);
                collect_local_bindings(&s.body, out);
                collect_local_bindings(&s.orelse, out);
            }
            Stmt::With(s) => {
                for item in &s.items {
                    if let Some(v) = &item.optional_vars {
                        names(v, out);
                    }
                }
                collect_local_bindings(&s.body, out);
            }
            Stmt::Try(s) => {
                collect_local_bindings(&s.body, out);
                collect_local_bindings(&s.orelse, out);
                collect_local_bindings(&s.finalbody, out);
                for h in &s.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    if let Some(n) = &h.name {
                        out.insert(n.as_str());
                    }
                    collect_local_bindings(&h.body, out);
                }
            }
            Stmt::Match(s) => {
                for case in &s.cases {
                    collect_local_bindings(&case.body, out);
                }
            }
            _ => {}
        }
    }
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
        let src = "xs: list[int] = []\nys: list[int] = [f(x) for x in xs]\n";
        let m = parse(src);
        let diags =
            parallel_opportunity_diagnostics(&m, "x.ty", src, &pure_set(&["f"]), 0, false, false);
        assert_eq!(codes(&diags).len(), 1);
        assert!(codes(&diags)[0].contains("parallel_opportunity"));
    }

    #[test]
    fn parallel_opportunity_silent_for_comprehension_when_on() {
        let src = "xs: list[int] = []\nys: list[int] = [f(x) for x in xs]\n";
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
    fn shared_mut_flags_in_place_mutation_and_helpers() {
        // 2026-10-03 review §7.9: subscript stores, mutating method calls and
        // a helper writing on the task's behalf all race with the spawner.
        for (body, why) in [
            ("    SEEN[k] = True\n", "subscript store"),
            ("    LOG.append(k)\n", "mutating method"),
            ("    del SEEN[k]\n", "item delete"),
            ("    Config.level = 3\n", "class attribute store"),
            ("    record(k)\n", "helper that mutates"),
        ] {
            let src = format!(
                "\
SEEN: dict[str, bool] = {{}}
LOG: list[str] = []

class Config:
    level: int = 0

def record(k: str) -> None:
    LOG.append(k)

async def worker(k: str) -> None:
{body}
async def main() -> None:
    go worker(\"a\")
"
            );
            let m = parse(&src);
            assert_eq!(
                codes(&shared_mut_across_tasks_diagnostics(&m, "x.ty", &src)).len(),
                1,
                "{why} must be flagged"
            );
        }
    }

    #[test]
    fn shared_mut_silent_for_local_and_parameter_mutation() {
        let src = "\
LOG: list[str] = []

def helper(xs: list[str]) -> None:
    xs.append(\"x\")

async def worker(items: list[str]) -> None:
    let LOG: list[str] = []
    LOG.append(\"a\")
    items.append(\"b\")
    helper(LOG)
    print(len(LOG))

async def main() -> None:
    go worker([])
";
        let m = parse(src);
        assert_eq!(
            codes(&shared_mut_across_tasks_diagnostics(&m, "x.ty", src)).len(),
            0,
            "a local shadow, a parameter and a helper mutating its argument are not module state"
        );
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

    #[test]
    fn shared_mut_silent_when_go_callee_is_a_parameter_or_local() {
        // The spawned callable is whatever the enclosing scope bound to the
        // name — here the argument (`quiet` at runtime), not the module-level
        // writer of the same name.
        let header = "\
import asyncio

mut HITS: int = 0

async def worker() -> None:
    global HITS
    HITS = HITS + 1

async def quiet() -> None:
    await asyncio.sleep(0)
";
        for body in [
            // parameter
            "async def launch(worker: Callable[[], Awaitable[None]]) -> None:\n    go worker()\n",
            // local assignment
            "async def launch() -> None:\n    worker = quiet\n    go worker()\n",
            // loop target
            "async def launch(ws: list[Callable[[], Awaitable[None]]]) -> None:\n    for worker in ws:\n        go worker()\n",
            // closure over an enclosing function's parameter
            "def outer(worker: Callable[[], Awaitable[None]]) -> None:\n    async def inner() -> None:\n        go worker()\n",
            // `with` target
            "async def launch(cm: Any) -> None:\n    with cm as worker:\n        go worker()\n",
        ] {
            let src = format!("{header}\n{body}");
            let m = parse(&src);
            assert_eq!(
                codes(&shared_mut_across_tasks_diagnostics(&m, "x.ty", &src)).len(),
                0,
                "a shadowed callee is not the module-level writer:\n{src}"
            );
        }
        // The review repro verbatim.
        let repro = "import asyncio\n\nmut HITS: int = 0\n\nasync def worker() -> None:\n    global HITS\n    HITS = HITS + 1\n\nasync def quiet() -> None:\n    await asyncio.sleep(0)\n\nasync def launch(worker: Callable[[], Awaitable[None]]) -> None:\n    go worker()\n    await asyncio.sleep(0.01)\n\nasync def main_async() -> None:\n    await launch(quiet)\n    await worker()\n\ndef main() -> None:\n    asyncio.run(main_async())\n    print(HITS)\n\nif __name__ == \"__main__\":\n    main()\n";
        let m = parse(repro);
        assert_eq!(
            codes(&shared_mut_across_tasks_diagnostics(&m, "x.ty", repro)).len(),
            0
        );
        // Controls: the unshadowed spawn still fires, also next to a
        // same-named binding in another function or a class body.
        for body in [
            "async def launch(fn: Callable[[], Awaitable[None]]) -> None:\n    go worker()\n",
            "async def other(worker: int) -> None:\n    pass\n\nasync def launch() -> None:\n    go worker()\n",
            "class Pool:\n    worker = 1\n\n    async def launch(self) -> None:\n        go worker()\n",
        ] {
            let src = format!("{header}\n{body}");
            let m = parse(&src);
            assert_eq!(
                codes(&shared_mut_across_tasks_diagnostics(&m, "x.ty", &src)).len(),
                1,
                "the module-level writer is spawned:\n{src}"
            );
        }
    }
}
