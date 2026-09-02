//! Purity, async, comptime, and optimisation analysis (Phase 2+).
//!
//! **Phase 2** implements `comptime` constant evaluation: bindings declared
//! with the `comptime` keyword have their RHS expressions evaluated at build
//! time by [`evaluate_comptime`].  Supported RHS forms:
//!
//! - Integer, float, string, and boolean literals.
//! - List / tuple / dict literals (`[1, 2]`, `(1, "a")`, `{"k": 1}`).
//! - Subscripting list / tuple / str by integer (with Python's
//!   negative-from-end semantics) and dict by any equality-comparable key.
//! - Unary operators (`-x`, `+x`, `not x`).
//! - Binary arithmetic (`+`, `-`, `*`, `/`, `%`, `//`, `**`, plus string
//!   concatenation with `+`).
//! - Comparisons (`==`, `!=`, `<`, `<=`, `>`, `>=`, plus chains).
//! - Short-circuit boolean operators (`and`, `or`).
//! - Ternary `x if cond else y`.
//! - `env("NAME")` — reads the environment variable `NAME`; fails the build
//!   if it is unset.
//! - `env("NAME", "default")` — reads `NAME` with a fallback value.
//! - `int(expr)`, `str(expr)`, `float(expr)` — type coercions on the above.
//! - `len(expr)` for str / list / tuple / dict.
//! - Pure `str` methods: `upper`, `lower`, `strip`, `lstrip`, `rstrip`,
//!   `replace`, `startswith`, `endswith`, `split`.
//! - Calls to user-defined `comptime def` functions (see
//!   [`evaluate_comptime_with_functions`]).
//!
//! The evaluator produces a [`HashMap`] from binding name to
//! [`ComptimeValue`].  The build command uses this map to substitute the
//! evaluated literals into the parsed AST before desugaring and emission.
//!
//! **Phase 3** adds purity inference and `@memo` / `@pure(memo=True)` opt-in
//! caching.  A function decorated with `@pure` (or `@pure(memo=True)`) must
//! satisfy every one of the six purity conditions:
//!
//! 1. Synchronous (no `async def`, no `yield`).
//! 2. All parameters have hashable types (only enforced loosely: `Any` is
//!    accepted but explicit mutable containers are not).
//! 3. No I/O calls in the body (`print`, `open`, logger calls, …).
//! 4. No reads from non-deterministic clocks or entropy sources.
//! 5. No reads from / writes to mutable module-level `var` state.  (Approximated
//!    here as "no `global` declarations and no assignment to module-level
//!    names found in the analyser's module-level binding set".)
//! 6. No `raise` statements — pure functions express failure via `Result[T, E]`.
//!
//! Violations produce a `TycError::ImpurePureFn` diagnostic.  When the user
//! also opts into memoisation (via `@memo`, `@pure(memo=True)`, or the
//! project-wide `[strictness] auto-memoise = true` toggle), the desugarer can
//! insert a `@functools.cache` / `@functools.lru_cache(maxsize=N)` decorator —
//! that emission lives in the desugar crate.

use std::collections::{HashMap, HashSet};

use ruff_python_ast::{Decorator, Expr, ExprCall, ModModule, Number, Parameters, Stmt};
use tyc_diagnostics::{Diagnostics, TycError};
use tyc_syntax::preprocess::ComptimeBinding;

/// Recursion limit for `comptime def` calls. Generous enough for any
/// realistic build-time configuration computation, low enough that an
/// infinite recursion bug terminates the build instead of hanging it.
const MAX_COMPTIME_DEPTH: usize = 64;

/// Cumulative budget (in bytes) for strings produced during a single comptime
/// evaluation. Integer arithmetic is already overflow-checked, but string
/// concatenation / replacement had no cap: within the depth limit a doubling
/// recursion (`grow(s + s, n)`) could allocate terabytes and abort `tyc check`
/// via the allocator, not a catchable diagnostic. 16 MiB is far above any real
/// build-time constant and stops a runaway build-string well before that.
const MAX_COMPTIME_STRING_BYTES: usize = 16 * 1024 * 1024;

pub mod auto_gather;
pub use auto_gather::{
    collect_gatherable_async_fn_names, detect_gather_opportunities, detect_missed_gathers,
    rewrite_auto_gather, AutoGatherStats, GatherOpportunity, MissedGather,
};

pub mod pgo;
pub use pgo::{load_profile_samples, pgo_memoise_targets, ProfileSample};

pub mod parallel;
pub use parallel::{
    detect_parallel_comprehensions, rewrite_parallel_comprehensions, ParallelStats,
};

pub mod reductions;
pub use reductions::{
    detect_reduction_loops, rewrite_reduction_loops, ReductionHit, ReductionStats,
};

pub mod parallel_lints;
pub use parallel_lints::{parallel_opportunity_diagnostics, shared_mut_across_tasks_diagnostics};

pub mod extend_builtin;
pub use extend_builtin::{
    extract_builtin_extensions, rewrite_builtin_extension_calls,
    rewrite_builtin_extension_calls_tracking, ExtensionExtractionStats, ExtensionRegistry,
};

pub mod perf;
pub use perf::{
    is_stdlib_top_level, lazy_import_opportunity_diagnostics, perf_diagnostics, PerfLintContext,
};

// ── Shared editor / CLI lint advisories ───────────────────────────────────────

/// Config knobs that gate two of the advisory lints. Mirrors the
/// matching `[strictness]` keys (`allow-secret-comptime`,
/// `suggest-gather`); defaults reproduce the default `typhon.toml`
/// behaviour so callers without a config (a standalone editor buffer)
/// still get the on-by-default advice.
#[derive(Debug, Clone, Copy)]
pub struct LintOptions {
    /// `[strictness] allow-secret-comptime` — when `true`, suppress the
    /// hard-coded-credential lint on plain `let` bindings.
    pub allow_secret_comptime: bool,
    /// `[strictness] suggest-gather` — when `true` (the default), surface
    /// `tyc::gather_opportunity` for runs of independent awaits.
    pub suggest_gather: bool,
    /// `[strictness] suggest-perf` — when `true` (the default), surface the
    /// `tyc::perf_*` / `tyc::lazy_import_opportunity` advice family.
    pub suggest_perf: bool,
    /// `[strictness] suggest-parallel` — when `true` (the default), surface
    /// `tyc::parallel_opportunity` and `tyc::shared_mut_across_tasks`. Only
    /// takes effect when [`LintOptions::free_threaded`] is also `true`.
    pub suggest_parallel: bool,
    /// `[python] free-threaded` — the free-threading advice lints
    /// (`suggest-parallel`) only fire when this is `true` (the project
    /// explicitly targets free-threaded Python). Defaults `false`, which keeps
    /// the two lints silent — and the corpus quiet — by construction.
    pub free_threaded: bool,
    /// `[strictness] auto-parallel` (resolved) — silences the
    /// `parallel_opportunity` comprehension arm when the rewrite is already on.
    pub auto_parallel: bool,
    /// `[strictness] auto-parallel-reductions` — silences the
    /// `parallel_opportunity` int-reduction arm when the rewrite is already on.
    pub auto_parallel_reductions: bool,
    /// `[strictness] parallel-min-size` — matches the rewrite's threshold so
    /// the advice fires on exactly the shapes that would be rewritten.
    pub parallel_min_size: u64,
}

impl Default for LintOptions {
    fn default() -> Self {
        // `allow-secret-comptime` defaults off (lint on); `suggest-gather`,
        // `suggest-perf`, and `suggest-parallel` default on — same as
        // `[strictness]`'s own defaults. `free-threaded` defaults off, so the
        // parallel advice lints stay silent unless a project opts in.
        Self {
            allow_secret_comptime: false,
            suggest_gather: true,
            suggest_perf: true,
            suggest_parallel: true,
            free_threaded: false,
            auto_parallel: false,
            auto_parallel_reductions: false,
            parallel_min_size: 64,
        }
    }
}

/// `tyc::gather_opportunity` advice for every run of 2+ adjacent
/// independent awaited calls in an `async def`. Wraps
/// [`detect_gather_opportunities`] so the diagnostic construction lives
/// in exactly one place (the `tyc check` command, the LSP, and the
/// `tyc build` nudge all route through this).
pub fn gather_opportunity_diagnostics(module: &ModModule, path: &str, source: &str) -> Diagnostics {
    let mut diags = Diagnostics::new();
    for opp in detect_gather_opportunities(module) {
        let offset = opp.call_range.start().to_usize();
        let length = opp
            .call_range
            .end()
            .to_usize()
            .saturating_sub(offset)
            .max(1);
        diags.push_warning(TycError::gather_opportunity(
            opp.count, path, source, offset, length,
        ));
    }
    diags
}

/// Run the pure-AST advisory lints that should fire identically in
/// `tyc check` and live in the editor (the LSP). Spans are byte offsets
/// into `source`, which must be the *preprocessed* Python the `module`
/// was parsed from. This is the single source of truth for the advisory
/// set, so a new lint added here lights up both surfaces at once
/// (purity and the import-vetting / `pub *` checks stay in the `check`
/// command — they need resolve / comptime context the LSP composes
/// separately).
///
/// `perf_ctx` carries the preprocess-derived facts the `tyc::perf_*`
/// family (specifically `lazy_import_opportunity`) needs — which imports
/// are already `lazy`, the module's `pub` names, and whether it has a
/// `pub *`. Pass [`perf::PerfLintContext::default`] when they're
/// unavailable (a standalone buffer); the perf family degrades gracefully.
pub fn editor_lint_diagnostics(
    module: &ModModule,
    path: &str,
    source: &str,
    opts: LintOptions,
    perf_ctx: &perf::PerfLintContext,
) -> Diagnostics {
    let mut diags = Diagnostics::new();
    // NOTE: `analyse_except_star_control_flow` is deliberately *not* called
    // here. It is an error, not a lint, so it runs on the shared check
    // pipeline in `tyc-db` — which `tyc build` also reaches, and this hook
    // does not. Calling it from both would double-report it in `tyc check`.
    diags.extend(analyse_empty_collection_bindings(module, path, source));
    diags.extend(analyse_typing_alias_annotations(module, path, source));
    diags.extend(analyse_mutable_default_params(module, path, source));
    diags.extend(analyse_is_literal_comparisons(module, path, source));
    diags.extend(analyse_loop_closure_captures(module, path, source));
    diags.extend(analyse_secret_literal_bindings(
        module,
        path,
        source,
        opts.allow_secret_comptime,
    ));
    if opts.suggest_gather {
        diags.extend(gather_opportunity_diagnostics(module, path, source));
    }
    if opts.suggest_perf {
        diags.extend(perf_diagnostics(module, path, source, perf_ctx));
    }
    // The free-threading advice lints (`parallel_opportunity`,
    // `shared_mut_across_tasks`) only fire when the project targets
    // free-threaded Python — the gate that keeps the corpus (which never sets
    // it) quiet by construction. The pure-callee set is only computed inside
    // this guard so free-threaded-off projects pay nothing for it.
    if opts.suggest_parallel && opts.free_threaded {
        diags.extend(shared_mut_across_tasks_diagnostics(module, path, source));
        let pure: std::collections::HashSet<String> = analyse_purity(module, false)
            .iter()
            .filter(|f| f.callable_as_pure())
            .map(|f| f.name.clone())
            .collect();
        diags.extend(parallel_opportunity_diagnostics(
            module,
            path,
            source,
            &pure,
            opts.parallel_min_size,
            opts.auto_parallel,
            opts.auto_parallel_reductions,
        ));
    }
    diags
}

// ── Public types ──────────────────────────────────────────────────────────────

/// A value that was determined at build time by evaluating a `comptime`
/// expression.
#[derive(Debug, Clone)]
pub enum ComptimeValue {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    /// `[a, b, c]` — order-preserving heterogeneous list.
    List(Vec<ComptimeValue>),
    /// `(a, b)` — same shape as List at the value level; only the
    /// emitted Python literal differs (parens vs brackets).
    Tuple(Vec<ComptimeValue>),
    /// `{"a": 1, "b": 2}` — order-preserving sequence of pairs.
    /// Stored as a Vec rather than a HashMap so insertion order is
    /// stable (Python dicts preserve insertion order from 3.7+, and
    /// reproducible emit is important for build determinism).
    Dict(Vec<(ComptimeValue, ComptimeValue)>),
    /// A type value — `comptime let T: type = int`.
    /// Stored as the type's display name (e.g., "int", "str", "list[int]").
    /// When used in an annotation slot, this round-trips as a type expression
    /// rather than a literal string.
    Type(String),
}

impl ComptimeValue {
    /// Render the value as a Python source literal (e.g. `42`, `"hello"`).
    pub fn to_python_literal(&self) -> String {
        match self {
            ComptimeValue::Int(n) => n.to_string(),
            // CPython's `repr` (shortest round-trip digits, exponent form
            // outside 1e-4 ..= 1e16) so the inlined literal is byte-for-byte
            // what the runtime would have printed. `inf` / `nan` are not
            // literals, so they are spelled as the calls that produce them.
            ComptimeValue::Float(f) if f.is_nan() => "float(\"nan\")".to_owned(),
            ComptimeValue::Float(f) if f.is_infinite() => if *f > 0.0 {
                "float(\"inf\")"
            } else {
                "-float(\"inf\")"
            }
            .to_owned(),
            ComptimeValue::Float(f) => python_float_repr(*f),
            ComptimeValue::Str(s) => python_string_literal(s),
            ComptimeValue::Bool(b) => if *b { "True" } else { "False" }.into(),
            ComptimeValue::List(xs) => {
                let mut s = String::from("[");
                for (i, v) in xs.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&v.to_python_literal());
                }
                s.push(']');
                s
            }
            ComptimeValue::Tuple(xs) => {
                let mut s = String::from("(");
                for (i, v) in xs.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&v.to_python_literal());
                }
                // Single-element tuples need the trailing comma.
                if xs.len() == 1 {
                    s.push(',');
                }
                s.push(')');
                s
            }
            ComptimeValue::Dict(items) => {
                let mut s = String::from("{");
                for (i, (k, v)) in items.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&k.to_python_literal());
                    s.push_str(": ");
                    s.push_str(&v.to_python_literal());
                }
                s.push('}');
                s
            }
            ComptimeValue::Type(type_name) => {
                // Type values are emitted as their type expression, not as string literals.
                // e.g., `int` not `"int"`, `list[str]` not `"list[str]"`.
                type_name.clone()
            }
        }
    }
}

// ── Comptime substitution ─────────────────────────────────────────────────────

/// Replace every `comptime let NAME: T = <expr>` initialiser in `module.body`
/// with the corresponding pre-evaluated literal from `values`, and strip
/// every `comptime def` body whose name appears in `comptime_fn_names` so
/// that the runtime never tries to evaluate it (those bodies often call
/// build-only intrinsics like `env(...)` that don't exist at runtime).
///
/// This is the same transformation the `tyc build` command applies before
/// desugaring; it is also called by the in-process VM (`tyc run`) so that
/// `comptime let X = ...` bindings work without a separate compile step.
pub fn substitute_comptime_literals(
    mut module: ModModule,
    values: &HashMap<String, ComptimeValue>,
    comptime_fn_names: &[String],
) -> ModModule {
    if values.is_empty() && comptime_fn_names.is_empty() {
        return module;
    }
    module.body = module
        .body
        .into_iter()
        .filter(|stmt| {
            if let Stmt::FunctionDef(f) = stmt {
                !comptime_fn_names.iter().any(|n| n == f.name.as_str())
            } else {
                true
            }
        })
        .map(|stmt| substitute_stmt(stmt, values))
        .collect();
    module
}

fn substitute_stmt(stmt: Stmt, values: &HashMap<String, ComptimeValue>) -> Stmt {
    if let Stmt::AnnAssign(mut ann) = stmt {
        if let Expr::Name(ref n) = *ann.target {
            if let Some(cv) = values.get(n.id.as_str()) {
                // B34: `comptime let T: type = int` is a build-time type
                // alias — the user's intent is for `T` to be substitutable
                // wherever a type appears, not for it to be a runtime
                // class named "T". Rewrite as `type T = int` (PEP 695
                // `TypeAliasStatement`) so the type-checker's existing
                // alias-resolution path picks it up automatically.
                if let ComptimeValue::Type(_) = cv {
                    let name_id = n.id.clone();
                    let value_expr = comptime_value_to_expr(cv);
                    return make_type_alias_stmt(&name_id, value_expr);
                }
                ann.value = Some(Box::new(comptime_value_to_expr(cv)));
                return Stmt::AnnAssign(ann);
            }
        }
        Stmt::AnnAssign(ann)
    } else {
        stmt
    }
}

/// Construct a PEP 695 `type NAME = VALUE` alias statement. Used by B34
/// to lower `comptime let T: type = int` after comptime evaluation.
fn make_type_alias_stmt(name: &ruff_python_ast::name::Name, value: Expr) -> Stmt {
    use ruff_python_ast::{AtomicNodeIndex, ExprName, StmtTypeAlias};
    use ruff_text_size::TextRange;
    let target = ExprName {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        id: name.clone(),
        ctx: ruff_python_ast::ExprContext::Store,
    };
    Stmt::TypeAlias(StmtTypeAlias {
        range: TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        name: Box::new(Expr::Name(target)),
        type_params: None,
        value: Box::new(value),
    })
}

fn comptime_value_to_expr(value: &ComptimeValue) -> Expr {
    use ruff_python_ast::{
        ExprStringLiteral, StringLiteral, StringLiteralFlags, StringLiteralValue,
    };
    use ruff_python_parser::parse_expression;
    use ruff_text_size::TextRange;
    let literal = value.to_python_literal();
    match parse_expression(&literal) {
        Ok(parsed) => *parsed.into_syntax().body,
        Err(_) => {
            // Fallback to a string literal — should never trip for the
            // value types we produce.
            let lit = StringLiteral {
                range: TextRange::default(),
                node_index: ruff_python_ast::AtomicNodeIndex::NONE,
                value: Box::from(literal.as_str()),
                flags: StringLiteralFlags::empty(),
            };
            Expr::StringLiteral(ExprStringLiteral {
                range: TextRange::default(),
                node_index: ruff_python_ast::AtomicNodeIndex::NONE,
                value: StringLiteralValue::single(lit),
            })
        }
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Evaluate all `comptime` bindings in `module` and return a map from binding
/// name to its computed value.
///
/// `bindings` comes from [`tyc_syntax::preprocess::PreprocessResult::comptime_bindings`].
///
/// Any binding whose RHS cannot be evaluated at compile time (e.g. a runtime
/// function call that is not `env(…)`) is skipped with a diagnostic error.
/// A missing required environment variable (`env("NAME")` without a default)
/// is also an error.
pub fn evaluate_comptime(
    module: &ModModule,
    bindings: &[ComptimeBinding],
) -> (HashMap<String, ComptimeValue>, Diagnostics) {
    evaluate_comptime_with_functions(module, bindings, &[])
}

/// Like [`evaluate_comptime`] but with a registry of `comptime def`
/// functions: any `Stmt::FunctionDef` in `module.body` whose name appears
/// in `comptime_function_names` is callable from a `comptime let` RHS.
///
/// Comptime functions follow a restricted contract — bodies support
/// `return EXPR`, local `NAME[: T] = EXPR` bindings, and `if`/`elif`/
/// `else` branches; expressions follow the same rules as a `comptime
/// let` initialiser (literals, arithmetic, string concatenation,
/// comparisons, boolean ops, ternaries, and calls to `env`/`int`/`str`/
/// `float` or other `comptime def` functions), with parameters bound to
/// the call's actual arguments. Recursion depth is capped at
/// [`MAX_COMPTIME_DEPTH`] so a buggy definition fails the build rather
/// than hanging it. See [`eval_stmt`] for the full statement contract.
pub fn evaluate_comptime_with_functions(
    module: &ModModule,
    bindings: &[ComptimeBinding],
    comptime_function_names: &[String],
) -> (HashMap<String, ComptimeValue>, Diagnostics) {
    let mut values = HashMap::new();
    let mut diags = Diagnostics::new();

    if bindings.is_empty() && comptime_function_names.is_empty() {
        return (values, diags);
    }

    let body = &module.body;
    let functions = collect_comptime_function_defs(body, comptime_function_names);

    for binding in bindings {
        let rhs = body.iter().find_map(|stmt| {
            if let Stmt::AnnAssign(a) = stmt {
                if let Expr::Name(n) = a.target.as_ref() {
                    if n.id.as_str() == binding.name {
                        return a.value.as_deref();
                    }
                }
            }
            None
        });

        match rhs {
            None => {
                // Distinguish "no value at all" from "value present but the
                // binding lacks the required type annotation" — the latter
                // lowers to a plain `Assign` (not `AnnAssign`), so the RHS
                // lookup above misses it and the old message ("no initialiser")
                // was misleading. A `comptime let` requires an explicit
                // annotation: `comptime let NAME: T = ...`.
                let has_unannotated_value = body.iter().any(|stmt| {
                    matches!(
                        stmt,
                        Stmt::Assign(a) if a.targets.iter().any(|t| {
                            matches!(t, Expr::Name(n) if n.id.as_str() == binding.name)
                        })
                    )
                });
                let message = if has_unannotated_value {
                    format!(
                        "comptime binding '{}' needs an explicit type annotation \
                         (write `comptime let {}: T = ...`)",
                        binding.name, binding.name
                    )
                } else {
                    format!("comptime binding '{}' has no initialiser", binding.name)
                };
                diags.push_error(TycError::comptime(binding.name.clone(), message));
            }
            Some(expr) => {
                let mut ctx = EvalContext::new(&functions);
                // Seed the evaluator's scope with previously-evaluated
                // comptime constants so a later binding can reference an
                // earlier one (FINDINGS #48). Bindings are evaluated in
                // source order, matching Python's left-to-right reading.
                for (name, value) in &values {
                    ctx.locals.insert(name.clone(), value.clone());
                }
                match eval_expr(expr, &mut ctx) {
                    Ok(v) => {
                        values.insert(binding.name.clone(), v);
                    }
                    Err(e) => {
                        diags.push_error(TycError::comptime(binding.name.clone(), e));
                    }
                }
            }
        }
    }

    (values, diags)
}

/// One `comptime def` registered in the function registry. Holds borrows
/// into the parsed module so the evaluator can read parameters and body
/// without cloning the AST.
struct ComptimeFnDef<'a> {
    name: &'a str,
    params: Vec<&'a str>,
    body: &'a [Stmt],
}

/// Per-evaluation context: a function registry shared across recursive
/// calls plus a local scope (parameter bindings, mutated when entering
/// a function call and restored on return) and a recursion-depth counter.
struct EvalContext<'a> {
    functions: &'a HashMap<&'a str, ComptimeFnDef<'a>>,
    locals: HashMap<String, ComptimeValue>,
    depth: usize,
    /// Running total of bytes allocated by string-producing operations across
    /// this whole evaluation (the context threads by `&mut` through recursion,
    /// so it accumulates). Charged against [`MAX_COMPTIME_STRING_BYTES`].
    string_bytes: usize,
}

impl<'a> EvalContext<'a> {
    fn new(functions: &'a HashMap<&'a str, ComptimeFnDef<'a>>) -> Self {
        Self {
            functions,
            locals: HashMap::new(),
            depth: 0,
            string_bytes: 0,
        }
    }

    /// Account for a freshly-produced string of `bytes` length; error out once
    /// the cumulative total crosses the budget so a runaway concatenation /
    /// replacement fails the build with a diagnostic instead of OOM-aborting.
    fn charge_string(&mut self, bytes: usize) -> Result<(), String> {
        self.string_bytes = self.string_bytes.saturating_add(bytes);
        if self.string_bytes > MAX_COMPTIME_STRING_BYTES {
            return Err(format!(
                "comptime string evaluation exceeded the {} MiB budget \
                 (a runaway concatenation or replacement?)",
                MAX_COMPTIME_STRING_BYTES / (1024 * 1024)
            ));
        }
        Ok(())
    }
}

/// Scan `body` for `Stmt::FunctionDef` nodes whose name appears in
/// `names`, extract their parameter list and body, and return a name →
/// definition map keyed by the parsed AST's name string slices (no
/// copying — the map borrows from the module).
///
/// Functions that fail validation (a non-name parameter pattern, missing
/// from the body, …) are silently dropped here. The first attempt to
/// call such a function from a comptime expression will fail with the
/// generic "unknown comptime function" diagnostic, which is fine: the
/// user's recourse is the same either way (fix the declaration).
fn collect_comptime_function_defs<'a>(
    body: &'a [Stmt],
    names: &[String],
) -> HashMap<&'a str, ComptimeFnDef<'a>> {
    let wanted: std::collections::HashSet<&str> = names.iter().map(|s| s.as_str()).collect();
    let mut out = HashMap::new();
    for stmt in body {
        if let Stmt::FunctionDef(f) = stmt {
            let name = f.name.as_str();
            if !wanted.contains(name) {
                continue;
            }
            // Pull parameter names. Reject any non-trivial parameter
            // form (defaults, *args, **kwargs, posonly, kwonly) — keeping
            // the surface tight makes the contract obvious and the
            // evaluator simple.
            let Some(params) = simple_parameter_names(&f.parameters) else {
                continue;
            };
            out.insert(
                name,
                ComptimeFnDef {
                    name,
                    params,
                    body: &f.body,
                },
            );
        }
    }
    out
}

/// Extract positional parameter names from a `Parameters` node, returning
/// `None` if any "fancy" form is used (defaults, *args, **kwargs, etc.).
/// The comptime evaluator only supports straight positional parameters.
fn simple_parameter_names(params: &Parameters) -> Option<Vec<&str>> {
    if params.vararg.is_some() || params.kwarg.is_some() {
        return None;
    }
    if !params.posonlyargs.is_empty() || !params.kwonlyargs.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(params.args.len());
    for p in &params.args {
        if p.default.is_some() {
            return None;
        }
        out.push(p.parameter.name.as_str());
    }
    Some(out)
}

// ── Expression evaluator ──────────────────────────────────────────────────────

fn eval_expr(expr: &Expr, ctx: &mut EvalContext<'_>) -> Result<ComptimeValue, String> {
    match expr {
        // Numeric literals.
        Expr::NumberLiteral(n) => match &n.value {
            Number::Int(i) => i
                .as_i64()
                .map(ComptimeValue::Int)
                .ok_or_else(|| format!("integer literal '{}' overflows i64", i)),
            Number::Float(f) => Ok(ComptimeValue::Float(*f)),
            Number::Complex { .. } => Err("complex literals are not comptime-evaluable".into()),
        },
        // String / boolean / none literals.
        Expr::StringLiteral(s) => Ok(ComptimeValue::Str(s.value.to_str().to_owned())),
        Expr::BooleanLiteral(b) => Ok(ComptimeValue::Bool(b.value)),
        Expr::NoneLiteral(_) => Err("None is not a valid comptime value".into()),

        // Name reference: looks up a parameter or local binding from the
        // current comptime function call frame. Free variables
        // (module-level names, including other `comptime let` bindings)
        // are intentionally rejected — comptime evaluation is hermetic,
        // so call sites must pass everything in as arguments.
        // Exception: bare type names (int, str, bool, float, bytes, etc.)
        // are recognized as `ComptimeValue::Type` values for comptime
        // types-as-values support.
        Expr::Name(n) => {
            // Check if it's a local binding first
            if let Some(val) = ctx.locals.get(n.id.as_str()) {
                return Ok(val.clone());
            }
            // Check if it's a bare type name (runtime-resolvable built-in types only).
            // `Any` is excluded because it's not a runtime builtin — emitting it without
            // importing from `typing` would cause NameError at runtime.
            match n.id.as_str() {
                "int" | "str" | "bool" | "float" | "bytes" | "None" | "type" | "object" => {
                    Ok(ComptimeValue::Type(n.id.to_string()))
                }
                _ => Err(format!(
                    "unknown name '{}' in comptime expression — only the enclosing function's \
                     parameters, locally-bound names, and runtime-resolvable built-in type names \
                     (int, str, bool, float, bytes, None, type, object) are in scope \
                     (comptime evaluation is hermetic)",
                    n.id
                )),
            }
        }

        // Unary operators: `-x`, `+x`, `not x`.
        Expr::UnaryOp(u) => {
            let operand = eval_expr(&u.operand, ctx)?;
            match (u.op, operand) {
                (ruff_python_ast::UnaryOp::USub, ComptimeValue::Int(n)) => n
                    .checked_neg()
                    .map(ComptimeValue::Int)
                    .ok_or_else(|| COMPTIME_INT_OVERFLOW("negation")),
                (ruff_python_ast::UnaryOp::USub, ComptimeValue::Float(f)) => {
                    Ok(ComptimeValue::Float(-f))
                }
                (ruff_python_ast::UnaryOp::UAdd, v @ ComptimeValue::Int(_)) => Ok(v),
                (ruff_python_ast::UnaryOp::UAdd, v @ ComptimeValue::Float(_)) => Ok(v),
                (ruff_python_ast::UnaryOp::Not, ComptimeValue::Bool(b)) => {
                    Ok(ComptimeValue::Bool(!b))
                }
                _ => Err("unary operator not supported on this comptime value".into()),
            }
        }

        // Function calls: env(), int(), str(), float(), or a registered
        // `comptime def`.
        Expr::Call(call) => eval_call(call, ctx),

        // Binary arithmetic on compile-time numerics and string concatenation.
        Expr::BinOp(b) => {
            let lhs = eval_expr(&b.left, ctx)?;
            let rhs = eval_expr(&b.right, ctx)?;
            let result = eval_binop(b.op, lhs, rhs)?;
            // A string concatenation is the one binop that grows an allocation
            // — charge its length so a doubling recursion (`grow(s + s, n)`)
            // hits the budget instead of OOM-aborting the build.
            if let ComptimeValue::Str(s) = &result {
                ctx.charge_string(s.len())?;
            }
            Ok(result)
        }

        // Comparison chains: `a < b`, `a == b`, `0 < n <= 10`. The
        // comparison is evaluated short-circuit, matching Python's
        // semantics — the first false comparator collapses the whole
        // chain to `False`.
        Expr::Compare(c) => eval_compare(c, ctx),

        // Boolean `and` / `or`: short-circuit on the first decisive
        // operand. The result of `a and b` is `b` if `a` is truthy and
        // `a` otherwise (matching Python's actual return values rather
        // than coercing to a plain bool).
        Expr::BoolOp(b) => eval_boolop(b, ctx),

        // Ternary `x if cond else y`. Condition must reduce to a Bool;
        // the other branch is only evaluated if its arm is selected,
        // matching short-circuit semantics.
        Expr::If(e) => {
            let cond = eval_expr(&e.test, ctx)?;
            match cond {
                ComptimeValue::Bool(true) => eval_expr(&e.body, ctx),
                ComptimeValue::Bool(false) => eval_expr(&e.orelse, ctx),
                _ => Err("comptime `if-expression` condition must reduce to a bool".into()),
            }
        }

        // Container literals: `[1, 2, 3]`, `(1, 2)`, `{"a": 1}`.
        // Empty containers are evaluable too — they're useful as
        // base cases (`comptime let TAGS: list[str] = []` etc.). A
        // Tuple with a single element parses through `Expr::Tuple`
        // even when written as `(x,)`.
        Expr::List(l) => {
            let mut elts = Vec::with_capacity(l.elts.len());
            for e in &l.elts {
                elts.push(eval_expr(e, ctx)?);
            }
            Ok(ComptimeValue::List(elts))
        }
        Expr::Tuple(t) => {
            let mut elts = Vec::with_capacity(t.elts.len());
            for e in &t.elts {
                elts.push(eval_expr(e, ctx)?);
            }
            Ok(ComptimeValue::Tuple(elts))
        }
        Expr::Dict(d) => {
            // Build with last-write-wins on duplicate keys, matching
            // Python's runtime dict semantics — `{"a": 1, "a": 2}` is
            // `{"a": 2}` at runtime and likewise here. Keeping the
            // dedup at construction time means `to_python_literal`
            // emits a literal that round-trips identically, and the
            // free `len()` correctly reports the unique-key count
            // rather than the source-pair count.
            let mut items: Vec<(ComptimeValue, ComptimeValue)> = Vec::with_capacity(d.items.len());
            for item in &d.items {
                let Some(key_expr) = item.key.as_ref() else {
                    // `**spread` in a dict literal — comptime evaluation
                    // doesn't model variadic spreads.
                    return Err(
                        "`**` dict spreading is not supported in comptime expressions".into(),
                    );
                };
                let k = eval_expr(key_expr, ctx)?;
                let v = eval_expr(&item.value, ctx)?;
                if let Some(existing) = items.iter_mut().find(|(ek, _)| values_equal(ek, &k)) {
                    existing.1 = v;
                } else {
                    items.push((k, v));
                }
            }
            Ok(ComptimeValue::Dict(items))
        }

        // Subscript: `E[0]`, `H["a"]`, `T[-1]`. Indexes list / tuple by
        // integer (with Python-style negative-from-end), dicts by any
        // comparable key. Slicing (`E[1:3]`) is not supported.
        Expr::Subscript(s) => {
            let receiver = eval_expr(&s.value, ctx)?;
            let key = eval_expr(&s.slice, ctx)?;
            eval_subscript(receiver, key)
        }

        // F-string: concatenate literal parts with the string form of
        // each interpolation. Format specs and conversion flags are not
        // supported — anyone reaching for `f"{x:>5}"` at comptime can
        // build the result with `str(...)` + `+` instead. Bare values
        // emit using the same surface as `to_python_literal` minus the
        // outer quotes, so `f"v{1}"` → `"v1"`, `f"{True}"` → `"True"`.
        Expr::FString(fs) => {
            let mut out = String::new();
            for part in fs.value.iter() {
                match part {
                    ruff_python_ast::FStringPart::Literal(lit) => {
                        out.push_str(lit.as_str());
                    }
                    ruff_python_ast::FStringPart::FString(inner) => {
                        for elem in &inner.elements {
                            match elem {
                                ruff_python_ast::InterpolatedStringElement::Literal(lit) => {
                                    out.push_str(&lit.value);
                                }
                                ruff_python_ast::InterpolatedStringElement::Interpolation(
                                    interp,
                                ) => {
                                    if interp.format_spec.is_some() {
                                        return Err(
                                            "f-string format specs are not supported in comptime \
                                             expressions"
                                                .into(),
                                        );
                                    }
                                    // `f"{expr=}"` renders the source text of
                                    // the field (`expr=`) followed by the
                                    // value's `repr` (`!s` switches to `str`).
                                    if let Some(debug) = &interp.debug_text {
                                        let v = eval_expr(&interp.expression, ctx)?;
                                        out.push_str(debug.as_str());
                                        let rendered = match interp.conversion.to_char() {
                                            None | Some('r') => comptime_repr(&v)?,
                                            Some('s') => comptime_str(&v)?,
                                            Some(_) => {
                                                return Err("f-string conversion flag `!a` is not supported in comptime expressions".into())
                                            }
                                        };
                                        out.push_str(&rendered);
                                        continue;
                                    }
                                    if interp.conversion.to_char().is_some() {
                                        return Err(
                                            "f-string conversion flags (`!r`, `!s`, `!a`) are not \
                                             supported in comptime expressions"
                                                .into(),
                                        );
                                    }
                                    let v = eval_expr(&interp.expression, ctx)?;
                                    out.push_str(&comptime_str(&v)?);
                                }
                            }
                        }
                    }
                }
            }
            Ok(ComptimeValue::Str(out))
        }

        // Attribute access exists only as part of a method call —
        // `"hi".upper()` parses as Call(Attribute(StringLiteral("hi"),
        // "upper"), []). The bare `Expr::Attribute` is handed off to
        // [`eval_call`] only via the call form; outside of that
        // context, attribute access on a comptime value has no
        // meaning yet (no records, no namespaced functions).
        other => Err(format!(
            "expression is not a comptime-evaluable constant: {}",
            expr_kind_name(other)
        )),
    }
}

/// String form of a comptime value as it would appear inside an
/// f-string interpolation. Best-effort match for Python's
/// `str(int)` / `str(bool)` / `str(str)`; for `str(float)` we use
/// Rust's default float formatting, which agrees with Python on the
/// common cases but diverges on a few pathological shapes
/// (exponent rendering, `inf` / `nan` casing). Container values
/// (`list`, `tuple`, `dict`) are explicitly rejected — Python's
/// `str([1, 2, 3])` matches our literal form, but a list containing
/// strings would emit double-quoted literals here while Python emits
/// single-quoted, so we'd produce a divergent constant. Unquoted,
/// since the surrounding f-string already provides the quotes
/// (codex / copilot reviews on PR #87).
fn comptime_str(v: &ComptimeValue) -> Result<String, String> {
    match v {
        ComptimeValue::Int(n) => Ok(n.to_string()),
        ComptimeValue::Float(f) => Ok(python_float_repr(*f)),
        ComptimeValue::Str(s) => Ok(s.clone()),
        ComptimeValue::Bool(b) => Ok(if *b { "True" } else { "False" }.to_owned()),
        ComptimeValue::Type(t) => Ok(python_type_str(t)),
        ComptimeValue::List(_) | ComptimeValue::Tuple(_) | ComptimeValue::Dict(_) => Err(
            "f-string interpolation of list/tuple/dict values is not supported at comptime — \
             Python's `str([...])` uses single-quoted string repr internally and the comptime \
             literal form uses double quotes, so the two would produce different constants"
                .to_owned(),
        ),
    }
}

/// Evaluate a Python comparison chain (`a < b`, `0 < n <= 10`, …) in a
/// comptime context. Returns `Bool(true)` only when every adjacent pair
/// satisfies its operator; short-circuits on the first false comparator.
fn eval_compare(
    c: &ruff_python_ast::ExprCompare,
    ctx: &mut EvalContext<'_>,
) -> Result<ComptimeValue, String> {
    let mut prev = eval_expr(&c.left, ctx)?;
    for (op, rhs_expr) in c.ops.iter().zip(c.comparators.iter()) {
        let rhs = eval_expr(rhs_expr, ctx)?;
        let outcome = eval_cmpop(*op, &prev, &rhs)?;
        if !outcome {
            return Ok(ComptimeValue::Bool(false));
        }
        prev = rhs;
    }
    Ok(ComptimeValue::Bool(true))
}

fn eval_cmpop(
    op: ruff_python_ast::CmpOp,
    lhs: &ComptimeValue,
    rhs: &ComptimeValue,
) -> Result<bool, String> {
    use ruff_python_ast::CmpOp::*;
    match (op, lhs, rhs) {
        (Eq, a, b) => Ok(values_equal(a, b)),
        (NotEq, a, b) => Ok(!values_equal(a, b)),
        (Lt | LtE | Gt | GtE, ComptimeValue::Str(a), ComptimeValue::Str(b)) => Ok(match op {
            Lt => a < b,
            LtE => a <= b,
            Gt => a > b,
            GtE => a >= b,
            _ => unreachable!(),
        }),
        // Two integers (incl. bool) compare exactly as i64 — no lossy f64
        // round-trip (`9007199254740993 > 9007199254740992` is True, but
        // both round to the same f64). Mixed int/float fall back to f64.
        (Lt | LtE | Gt | GtE, a, b) if cmp_num_int(a).is_some() && cmp_num_int(b).is_some() => {
            let (x, y) = (cmp_num_int(a).unwrap(), cmp_num_int(b).unwrap());
            Ok(match op {
                Lt => x < y,
                LtE => x <= y,
                Gt => x > y,
                GtE => x >= y,
                _ => unreachable!(),
            })
        }
        (Lt | LtE | Gt | GtE, a, b) => match (cmp_num_f64(a), cmp_num_f64(b)) {
            (Some(x), Some(y)) => Ok(match op {
                Lt => x < y,
                LtE => x <= y,
                Gt => x > y,
                GtE => x >= y,
                _ => unreachable!(),
            }),
            _ => Err("comptime ordering comparison requires two numerics or two strings".into()),
        },
        (other, _, _) => Err(format!(
            "comparison operator `{:?}` is not supported in comptime expressions",
            other
        )),
    }
}

/// Index into a comptime list / tuple / dict by an evaluated key.
///
/// Lists and tuples are indexed by integer with Python's negative-from-end
/// semantics (`-1` is the last element). Out-of-range indices produce an
/// error so the build fails loudly rather than silently shifting.
///
/// Dicts are indexed by any value that supports `==`-equality against the
/// stored keys. A missing key is an error.
///
/// Slicing (`E[1:3]`) is not supported — comptime slicing would require
/// modelling Python's `slice` object and adds little practical value for
/// build-time constant computation.
fn eval_subscript(receiver: ComptimeValue, key: ComptimeValue) -> Result<ComptimeValue, String> {
    match (receiver, key) {
        (ComptimeValue::List(items), ComptimeValue::Int(i))
        | (ComptimeValue::Tuple(items), ComptimeValue::Int(i)) => {
            let len = items.len() as i64;
            let idx = if i < 0 { i + len } else { i };
            if idx < 0 || idx >= len {
                return Err(format!(
                    "comptime index {} is out of range for a sequence of length {}",
                    i, len
                ));
            }
            Ok(items[idx as usize].clone())
        }
        (ComptimeValue::Dict(items), k) => items
            .into_iter()
            .find_map(|(ek, v)| if values_equal(&ek, &k) { Some(v) } else { None })
            .ok_or_else(|| "comptime dict has no matching key".to_string()),
        (ComptimeValue::Str(s), ComptimeValue::Int(i)) => {
            let chars: Vec<char> = s.chars().collect();
            let len = chars.len() as i64;
            let idx = if i < 0 { i + len } else { i };
            if idx < 0 || idx >= len {
                return Err(format!(
                    "comptime index {} is out of range for a string of length {}",
                    i, len
                ));
            }
            Ok(ComptimeValue::Str(chars[idx as usize].to_string()))
        }
        (recv, _) => Err(format!(
            "comptime subscript not supported on {}",
            comptime_value_kind(&recv)
        )),
    }
}

fn comptime_value_kind(v: &ComptimeValue) -> &'static str {
    match v {
        ComptimeValue::Int(_) => "int",
        ComptimeValue::Float(_) => "float",
        ComptimeValue::Str(_) => "str",
        ComptimeValue::Bool(_) => "bool",
        ComptimeValue::List(_) => "list",
        ComptimeValue::Tuple(_) => "tuple",
        ComptimeValue::Dict(_) => "dict",
        ComptimeValue::Type(_) => "type",
    }
}

/// Numeric view of a value treating `bool` as `0`/`1` — Python's `bool` is a
/// subclass of `int`, so `True == 1` / `True < 2` must hold at comptime.
fn cmp_num_int(v: &ComptimeValue) -> Option<i64> {
    match v {
        ComptimeValue::Int(n) => Some(*n),
        ComptimeValue::Bool(b) => Some(*b as i64),
        _ => None,
    }
}
fn cmp_num_f64(v: &ComptimeValue) -> Option<f64> {
    match v {
        ComptimeValue::Int(n) => Some(*n as f64),
        ComptimeValue::Float(f) => Some(*f),
        ComptimeValue::Bool(b) => Some(*b as i64 as f64),
        _ => None,
    }
}

fn values_equal(a: &ComptimeValue, b: &ComptimeValue) -> bool {
    match (a, b) {
        (ComptimeValue::Str(x), ComptimeValue::Str(y)) => x == y,
        (ComptimeValue::Type(x), ComptimeValue::Type(y)) => x == y,
        // Containers compare structurally, like Python: element-wise for
        // lists and tuples (never across the two kinds), key/value-wise for
        // dicts regardless of insertion order.
        (ComptimeValue::List(x), ComptimeValue::List(y))
        | (ComptimeValue::Tuple(x), ComptimeValue::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| values_equal(p, q))
        }
        (ComptimeValue::Dict(x), ComptimeValue::Dict(y)) => {
            x.len() == y.len()
                && x.iter().all(|(k, v)| {
                    y.iter()
                        .any(|(k2, v2)| values_equal(k, k2) && values_equal(v, v2))
                })
        }
        (ComptimeValue::List(_) | ComptimeValue::Tuple(_) | ComptimeValue::Dict(_), _)
        | (_, ComptimeValue::List(_) | ComptimeValue::Tuple(_) | ComptimeValue::Dict(_)) => false,
        // Numeric (int/float/bool) equality. `bool` folds into `int`
        // (`True == 1`); two integers compare exactly (no lossy f64 round-trip).
        _ => match (cmp_num_int(a), cmp_num_int(b)) {
            (Some(x), Some(y)) => x == y,
            _ => match (cmp_num_f64(a), cmp_num_f64(b)) {
                (Some(x), Some(y)) => x == y,
                _ => false,
            },
        },
    }
}

/// Evaluate Python's short-circuiting `and` / `or` with Python's actual
/// return-value semantics: `a and b` is `b` when `a` is truthy and `a`
/// otherwise; `a or b` is `a` when `a` is truthy and `b` otherwise.
/// Only Bool operands are accepted in v1 — promoting numeric or string
/// values to a truthiness rule would surprise readers.
fn eval_boolop(
    b: &ruff_python_ast::ExprBoolOp,
    ctx: &mut EvalContext<'_>,
) -> Result<ComptimeValue, String> {
    use ruff_python_ast::BoolOp::*;
    let mut last = ComptimeValue::Bool(matches!(b.op, And));
    for operand in &b.values {
        let v = eval_expr(operand, ctx)?;
        let truthy = match v {
            ComptimeValue::Bool(x) => x,
            _ => return Err("comptime `and`/`or` operands must be booleans".into()),
        };
        last = v;
        match (b.op, truthy) {
            (And, false) => return Ok(last),
            (Or, true) => return Ok(last),
            _ => {}
        }
    }
    Ok(last)
}

fn eval_call(call: &ExprCall, ctx: &mut EvalContext<'_>) -> Result<ComptimeValue, String> {
    // Method-call form: `RECEIVER.METHOD(args)`. Currently supports a
    // small set of pure string methods (`upper`, `lower`, `strip`,
    // `replace`, `split`, `startswith`, `endswith`, `len`-via-`__len__`-
    // free); the broader Python str API is intentionally narrow because
    // comptime evaluation is hermetic and every method here must be
    // determinable from inputs alone.
    if let Expr::Attribute(attr) = call.func.as_ref() {
        let receiver = eval_expr(&attr.value, ctx)?;
        return eval_method_call(receiver, attr.attr.as_str(), call, ctx);
    }

    let func_name = match call.func.as_ref() {
        Expr::Name(n) => n.id.as_str(),
        _ => return Err(
            "only simple function calls (env, int, str, float, or a `comptime def`) are valid in comptime expressions"
                .into(),
        ),
    };

    let args = &call.arguments.args;
    let keywords = &call.arguments.keywords;

    match func_name {
        "env" => eval_env_call(call, ctx),
        "int" => {
            if args.len() != 1 || !keywords.is_empty() {
                return Err("int() in comptime context takes exactly one argument".into());
            }
            match eval_expr(&args[0], ctx)? {
                ComptimeValue::Str(s) => s
                    .trim()
                    .parse::<i64>()
                    .map(ComptimeValue::Int)
                    .map_err(|_| format!("cannot parse '{}' as int", s)),
                ComptimeValue::Int(n) => Ok(ComptimeValue::Int(n)),
                // Rust's `as i64` *saturates* at `i64::MIN`/`i64::MAX` and maps
                // NaN to 0, so `int(1e30)` silently folded the constant
                // 9223372036854775807 into the build and `int(float("inf"))`
                // did the same instead of raising. A comptime value is inlined
                // as a literal, so a wrong one is baked into the artifact with
                // nothing downstream able to notice.
                ComptimeValue::Float(f) => {
                    if f.is_nan() {
                        return Err("cannot convert float NaN to integer".into());
                    }
                    if f.is_infinite() {
                        return Err("cannot convert float infinity to integer".into());
                    }
                    let truncated = f.trunc();
                    if truncated < i64::MIN as f64 || truncated > i64::MAX as f64 {
                        return Err(format!(
                            "int({f}) is out of range for a comptime integer constant"
                        ));
                    }
                    Ok(ComptimeValue::Int(truncated as i64))
                }
                ComptimeValue::Bool(b) => Ok(ComptimeValue::Int(if b { 1 } else { 0 })),
                ComptimeValue::List(_)
                | ComptimeValue::Tuple(_)
                | ComptimeValue::Dict(_)
                | ComptimeValue::Type(_) => {
                    Err("int() in comptime context cannot accept a container or type".into())
                }
            }
        }
        "float" => {
            if args.len() != 1 || !keywords.is_empty() {
                return Err("float() in comptime context takes exactly one argument".into());
            }
            match eval_expr(&args[0], ctx)? {
                ComptimeValue::Str(s) => s
                    .trim()
                    .parse::<f64>()
                    .map(ComptimeValue::Float)
                    .map_err(|_| format!("cannot parse '{}' as float", s)),
                ComptimeValue::Int(n) => Ok(ComptimeValue::Float(n as f64)),
                ComptimeValue::Float(f) => Ok(ComptimeValue::Float(f)),
                ComptimeValue::Bool(b) => Ok(ComptimeValue::Float(if b { 1.0 } else { 0.0 })),
                ComptimeValue::List(_)
                | ComptimeValue::Tuple(_)
                | ComptimeValue::Dict(_)
                | ComptimeValue::Type(_) => {
                    Err("float() in comptime context cannot accept a container or type".into())
                }
            }
        }
        "str" => {
            if args.len() != 1 || !keywords.is_empty() {
                return Err("str() in comptime context takes exactly one argument".into());
            }
            let v = eval_expr(&args[0], ctx)?;
            Ok(ComptimeValue::Str(match v {
                ComptimeValue::Str(s) => s,
                ComptimeValue::Int(n) => n.to_string(),
                // Python-faithful float string: CPython's `repr` — `str(2.0)`
                // is `"2.0"`, `str(1e16)` is `"1e+16"`, `str(1e-5)` is
                // `"1e-05"` — so the inlined constant is what the runtime
                // would have printed.
                ComptimeValue::Float(f) => python_float_repr(f),
                ComptimeValue::Bool(b) => if b { "True" } else { "False" }.into(),
                // `str(int)` is `"<class 'int'>"`; a parameterised alias
                // (`list[int]`) and `None` print as themselves.
                ComptimeValue::Type(t) => python_type_str(&t),
                // Reject `str(container)` at comptime: matching Python's
                // `str(["a"])` -> `"['a']"` (single-quoted nested strings) would
                // require a separate Python-flavoured repr serialiser, and the
                // double-quoted literal form would differ from the runtime
                // result by one character per nested string.
                ComptimeValue::List(_)
                | ComptimeValue::Tuple(_)
                | ComptimeValue::Dict(_) => {
                    return Err(
                        "str() on a container in comptime context would not match Python's runtime \
                         repr (single-quoted nested strings); fold to a literal explicitly instead"
                            .into(),
                    );
                }
            }))
        }
        "len" => {
            if args.len() != 1 || !keywords.is_empty() {
                return Err("len() in comptime context takes exactly one argument".into());
            }
            match eval_expr(&args[0], ctx)? {
                ComptimeValue::Str(s) => Ok(ComptimeValue::Int(s.chars().count() as i64)),
                ComptimeValue::List(xs) | ComptimeValue::Tuple(xs) => {
                    Ok(ComptimeValue::Int(xs.len() as i64))
                }
                // Dict items are unique-keyed at construction time (see
                // `Expr::Dict` arm in `eval_expr`), so the Vec length
                // matches Python's `len(d)`.
                ComptimeValue::Dict(items) => Ok(ComptimeValue::Int(items.len() as i64)),
                _ => Err("len() requires a str, list, tuple, or dict".into()),
            }
        }
        // Dispatch into a user-defined `comptime def` if one matches.
        _ if ctx.functions.contains_key(func_name) => {
            if !keywords.is_empty() {
                return Err(format!(
                    "comptime function '{func_name}' does not accept keyword arguments"
                ));
            }
            eval_user_comptime_call(func_name, args, ctx)
        }
        other => Err(format!(
            "function '{}' is not valid in a comptime expression; use env(), int(), str(), float(), \
             or a top-level `comptime def`",
            other
        )),
    }
}

/// Dispatch a method call on an already-evaluated comptime value.
/// Today supports a tight, pure-only subset of the Python string and
/// container APIs — every method has a deterministic value-from-value
/// contract (no I/O, no locale, no randomness). Reject everything
/// else with a generic "unsupported" message so users discover the
/// limit without combinatorial diagnostic per method name.
fn eval_method_call(
    receiver: ComptimeValue,
    method: &str,
    call: &ExprCall,
    ctx: &mut EvalContext<'_>,
) -> Result<ComptimeValue, String> {
    if !call.arguments.keywords.is_empty() {
        return Err(format!(
            "comptime method '{method}' does not accept keyword arguments"
        ));
    }
    let args: Vec<ComptimeValue> = call
        .arguments
        .args
        .iter()
        .map(|a| eval_expr(a, ctx))
        .collect::<Result<_, _>>()?;
    match (&receiver, method) {
        // ── str methods ──────────────────────────────────────────────
        (ComptimeValue::Str(s), "upper") => {
            expect_arity(method, 0, &args)?;
            Ok(ComptimeValue::Str(s.to_uppercase()))
        }
        (ComptimeValue::Str(s), "lower") => {
            expect_arity(method, 0, &args)?;
            Ok(ComptimeValue::Str(s.to_lowercase()))
        }
        (ComptimeValue::Str(s), "strip") => {
            expect_arity(method, 0, &args)?;
            Ok(ComptimeValue::Str(s.trim().to_owned()))
        }
        (ComptimeValue::Str(s), "lstrip") => {
            expect_arity(method, 0, &args)?;
            Ok(ComptimeValue::Str(s.trim_start().to_owned()))
        }
        (ComptimeValue::Str(s), "rstrip") => {
            expect_arity(method, 0, &args)?;
            Ok(ComptimeValue::Str(s.trim_end().to_owned()))
        }
        (ComptimeValue::Str(s), "replace") => {
            expect_arity(method, 2, &args)?;
            let needle = expect_str(method, &args[0])?;
            let replacement = expect_str(method, &args[1])?;
            let out = s.replace(needle, replacement);
            // `replace` can grow the string (a longer replacement than needle);
            // charge it so a recursive `grow(s.replace(a, aaaa), n)` hits the
            // budget rather than OOM-aborting.
            ctx.charge_string(out.len())?;
            Ok(ComptimeValue::Str(out))
        }
        (ComptimeValue::Str(s), "startswith") => {
            expect_arity(method, 1, &args)?;
            let prefix = expect_str(method, &args[0])?;
            Ok(ComptimeValue::Bool(s.starts_with(prefix)))
        }
        (ComptimeValue::Str(s), "endswith") => {
            expect_arity(method, 1, &args)?;
            let suffix = expect_str(method, &args[0])?;
            Ok(ComptimeValue::Bool(s.ends_with(suffix)))
        }
        (ComptimeValue::Str(s), "split") => {
            expect_arity(method, 1, &args)?;
            let sep = expect_str(method, &args[0])?;
            if sep.is_empty() {
                return Err("comptime str.split() requires a non-empty separator".into());
            }
            let parts: Vec<ComptimeValue> = s
                .split(sep)
                .map(|p| ComptimeValue::Str(p.to_owned()))
                .collect();
            Ok(ComptimeValue::List(parts))
        }
        // N3 (2026-05-22): pair `split` with `join`. Common pattern:
        // a comptime-derived list of tags from an env var, then a
        // canonical comma-joined string to embed in a banner.
        (ComptimeValue::Str(sep), "join") => {
            expect_arity(method, 1, &args)?;
            // Accept a list or tuple of strings.
            let items: Vec<&str> = match &args[0] {
                ComptimeValue::List(xs) | ComptimeValue::Tuple(xs) => {
                    let mut strs: Vec<&str> = Vec::with_capacity(xs.len());
                    for x in xs {
                        match x {
                            ComptimeValue::Str(s) => strs.push(s.as_str()),
                            _ => {
                                return Err(
                                    "comptime str.join() requires an iterable of str — \
                                     a non-string element appeared"
                                        .into(),
                                )
                            }
                        }
                    }
                    strs
                }
                _ => {
                    return Err(
                        "comptime str.join() requires a list / tuple of str".into(),
                    )
                }
            };
            Ok(ComptimeValue::Str(items.join(sep)))
        }
        (ComptimeValue::Str(_), other) => Err(format!(
            "comptime str method '{other}' is not supported; available: upper, lower, strip, lstrip, rstrip, replace, startswith, endswith, split, join"
        )),

        // ── unsupported receiver ─────────────────────────────────────
        // Note: list / tuple / dict have no comptime methods. `len()`
        // is exposed as the free function in `eval_call`, matching
        // Python's `len(x)` rather than the non-Pythonic `x.len()`.
        _ => Err(format!(
            "comptime method '{method}' is not supported on this value type"
        )),
    }
}

fn expect_arity(method: &str, expected: usize, args: &[ComptimeValue]) -> Result<(), String> {
    if args.len() != expected {
        return Err(format!(
            "comptime method '{method}' expects {expected} argument(s), got {}",
            args.len()
        ));
    }
    Ok(())
}

fn expect_str<'a>(method: &str, v: &'a ComptimeValue) -> Result<&'a str, String> {
    match v {
        ComptimeValue::Str(s) => Ok(s.as_str()),
        _ => Err(format!(
            "comptime method '{method}' expects a string argument"
        )),
    }
}

/// Dispatch a call into a user-declared `comptime def` function. Binds
/// each evaluated argument to the matching parameter name in a fresh
/// scope, recurses on the body, and restores the caller's locals on
/// return. Recursion depth is capped to avoid hanging the build on a
/// definition that recurses without a base case.
fn eval_user_comptime_call(
    name: &str,
    args: &[Expr],
    ctx: &mut EvalContext<'_>,
) -> Result<ComptimeValue, String> {
    let def = ctx
        .functions
        .get(name)
        .expect("caller already checked the registry");

    if args.len() != def.params.len() {
        return Err(format!(
            "comptime function '{}' expects {} argument(s), got {}",
            def.name,
            def.params.len(),
            args.len()
        ));
    }

    if ctx.depth >= MAX_COMPTIME_DEPTH {
        return Err(format!(
            "comptime call depth exceeded {} while evaluating '{}' (recursive comptime function?)",
            MAX_COMPTIME_DEPTH, name
        ));
    }

    // Evaluate arguments in the caller's scope first; binding into the
    // callee's scope only after all args are computed avoids leaking
    // partially-bound state if one of them errors out.
    let mut evaluated = Vec::with_capacity(args.len());
    for arg in args {
        evaluated.push(eval_expr(arg, ctx)?);
    }

    // Swap in a fresh locals frame so callee parameters don't shadow the
    // caller's bindings in a way the caller can observe on return.
    let previous_locals = std::mem::take(&mut ctx.locals);
    for (param, value) in def.params.iter().zip(evaluated.into_iter()) {
        ctx.locals.insert((*param).to_owned(), value);
    }
    ctx.depth += 1;

    let result = eval_function_body(def.body, ctx);

    // Restore caller state. Must happen even when the call errors out.
    ctx.depth -= 1;
    ctx.locals = previous_locals;

    result.map_err(|e| format!("in comptime call to '{name}': {e}"))
}

/// Outcome of executing a statement (or sequence) inside a comptime
/// function body: either control fell through to the next statement, or
/// the function `return`ed with a value.
enum StmtOutcome {
    Returned(ComptimeValue),
    FellThrough,
}

/// Evaluate a comptime function body and return the value it `return`ed.
/// Supported statement shapes:
///
/// - `return EXPR`,
/// - `NAME[: T] = EXPR` (with or without annotation) — binds a local,
/// - `if COND: ... elif/else: ...` — branches on a boolean condition,
/// - a leading docstring is tolerated.
///
/// Loops, exceptions, with-blocks, and class/def declarations are not
/// supported in v1; calling one is a hard error. Walking off the end of
/// the body without a `return` is also an error — a comptime function
/// must produce a value.
fn eval_function_body(body: &[Stmt], ctx: &mut EvalContext<'_>) -> Result<ComptimeValue, String> {
    let mut start = 0;
    if let Some(Stmt::Expr(e)) = body.first() {
        if matches!(e.value.as_ref(), Expr::StringLiteral(_)) {
            start = 1;
        }
    }
    match eval_stmts(&body[start..], ctx)? {
        StmtOutcome::Returned(v) => Ok(v),
        StmtOutcome::FellThrough => {
            Err("comptime function fell through without `return`ing a value".into())
        }
    }
}

fn eval_stmts(stmts: &[Stmt], ctx: &mut EvalContext<'_>) -> Result<StmtOutcome, String> {
    for stmt in stmts {
        match eval_stmt(stmt, ctx)? {
            StmtOutcome::FellThrough => continue,
            ret @ StmtOutcome::Returned(_) => return Ok(ret),
        }
    }
    Ok(StmtOutcome::FellThrough)
}

fn eval_stmt(stmt: &Stmt, ctx: &mut EvalContext<'_>) -> Result<StmtOutcome, String> {
    match stmt {
        Stmt::Return(r) => {
            let Some(value) = r.value.as_deref() else {
                return Err(
                    "comptime function must `return` a value (bare `return` is not valid)".into(),
                );
            };
            Ok(StmtOutcome::Returned(eval_expr(value, ctx)?))
        }
        Stmt::Assign(a) => {
            if a.targets.len() != 1 {
                return Err("comptime function does not support multi-target assignment".into());
            }
            let Expr::Name(n) = &a.targets[0] else {
                return Err("comptime function only supports `NAME = EXPR` assignments".into());
            };
            let value = eval_expr(&a.value, ctx)?;
            ctx.locals.insert(n.id.as_str().to_owned(), value);
            Ok(StmtOutcome::FellThrough)
        }
        Stmt::AnnAssign(a) => {
            let Expr::Name(n) = a.target.as_ref() else {
                return Err(
                    "comptime function only supports `NAME[: T] = EXPR` annotated assignments"
                        .into(),
                );
            };
            let Some(rhs) = a.value.as_deref() else {
                return Err(
                    "annotated declaration inside a comptime function must have an initialiser"
                        .into(),
                );
            };
            let value = eval_expr(rhs, ctx)?;
            ctx.locals.insert(n.id.as_str().to_owned(), value);
            Ok(StmtOutcome::FellThrough)
        }
        Stmt::If(i) => {
            let cond = eval_expr(&i.test, ctx)?;
            let truthy = match cond {
                ComptimeValue::Bool(b) => b,
                _ => return Err("comptime `if` condition must reduce to a bool".into()),
            };
            if truthy {
                return eval_stmts(&i.body, ctx);
            }
            // Python's AST encodes `elif`/`else` as a list of clauses;
            // the first one with `test = None` is the `else`.
            for clause in &i.elif_else_clauses {
                match &clause.test {
                    Some(test) => {
                        let cond = eval_expr(test, ctx)?;
                        let truthy = match cond {
                            ComptimeValue::Bool(b) => b,
                            _ => {
                                return Err("comptime `elif` condition must reduce to a bool".into())
                            }
                        };
                        if truthy {
                            return eval_stmts(&clause.body, ctx);
                        }
                    }
                    None => return eval_stmts(&clause.body, ctx),
                }
            }
            Ok(StmtOutcome::FellThrough)
        }
        // Bare expression statements (other than a leading docstring,
        // which `eval_function_body` peels off before delegating here)
        // are rejected. Silently skipping them would let a body like
        // `1 / 0; return 1` produce the literal `1` at compile time
        // even though the same code emitted as a Python `def` would
        // crash at runtime — divergence between comptime constants
        // and runtime behaviour is exactly the kind of bug the
        // hermetic-evaluator contract is supposed to prevent.
        Stmt::Expr(_) => Err(
            "bare expression statement is not supported inside a comptime function body \
             (only a leading docstring is allowed)"
                .into(),
        ),
        other => Err(format!(
            "`{}` is not supported inside a comptime function body (v1 supports \
             `return`, local assignments, and `if`/`elif`/`else`)",
            stmt_kind_name(other)
        )),
    }
}

fn eval_env_call(call: &ExprCall, ctx: &mut EvalContext<'_>) -> Result<ComptimeValue, String> {
    let args = &call.arguments.args;
    let keywords = &call.arguments.keywords;
    if args.is_empty() || args.len() > 2 {
        return Err("env() requires one or two positional arguments: env(\"NAME\") or env(\"NAME\", \"default\")".into());
    }
    if !keywords.is_empty() {
        return Err("env() does not accept keyword arguments".into());
    }

    let var_name = match eval_expr(&args[0], ctx)? {
        ComptimeValue::Str(s) => s,
        _ => return Err("env() first argument must be a string literal".into()),
    };

    match std::env::var(&var_name) {
        Ok(val) => Ok(ComptimeValue::Str(val)),
        Err(_) => {
            if args.len() == 2 {
                // Use the default value.
                eval_expr(&args[1], ctx)
            } else {
                Err(format!(
                    "required environment variable '{}' is not set",
                    var_name
                ))
            }
        }
    }
}

/// Coerce a numeric comptime value to `f64`. Only call on `Int`/`Float`.
fn comptime_as_f64(v: &ComptimeValue) -> f64 {
    match v {
        ComptimeValue::Int(n) => *n as f64,
        ComptimeValue::Float(f) => *f,
        _ => f64::NAN,
    }
}

/// Message for a comptime integer overflow. Comptime arithmetic is evaluated
/// in 64-bit (unlike Typhon's arbitrary-precision runtime `int`), so a value
/// that exceeds `i64` can't be a build-time constant. The hint points at the
/// runtime alternative so the limitation is actionable rather than mysterious.
#[allow(non_snake_case)]
fn COMPTIME_INT_OVERFLOW(op: &str) -> String {
    format!(
        "integer overflow in comptime {op} — comptime arithmetic is 64-bit, \
         so a value this large can't be a build-time constant; compute it at \
         runtime instead (a normal `let` / `lazy let`, not `comptime let`)"
    )
}

fn eval_binop(
    op: ruff_python_ast::Operator,
    lhs: ComptimeValue,
    rhs: ComptimeValue,
) -> Result<ComptimeValue, String> {
    use ruff_python_ast::Operator::*;
    match (op, &lhs, &rhs) {
        // ── Add ──────────────────────────────────────────────────────────────
        (Add, ComptimeValue::Int(a), ComptimeValue::Int(b)) => a
            .checked_add(*b)
            .map(ComptimeValue::Int)
            .ok_or_else(|| COMPTIME_INT_OVERFLOW("addition")),
        (Add, ComptimeValue::Float(a), ComptimeValue::Float(b)) => Ok(ComptimeValue::Float(a + b)),
        (Add, ComptimeValue::Int(a), ComptimeValue::Float(b)) => {
            Ok(ComptimeValue::Float(*a as f64 + b))
        }
        (Add, ComptimeValue::Float(a), ComptimeValue::Int(b)) => {
            Ok(ComptimeValue::Float(a + *b as f64))
        }
        (Add, ComptimeValue::Str(a), ComptimeValue::Str(b)) => {
            Ok(ComptimeValue::Str(format!("{}{}", a, b)))
        }
        // ── Sub ──────────────────────────────────────────────────────────────
        (Sub, ComptimeValue::Int(a), ComptimeValue::Int(b)) => a
            .checked_sub(*b)
            .map(ComptimeValue::Int)
            .ok_or_else(|| COMPTIME_INT_OVERFLOW("subtraction")),
        (Sub, ComptimeValue::Float(a), ComptimeValue::Float(b)) => Ok(ComptimeValue::Float(a - b)),
        (Sub, ComptimeValue::Int(a), ComptimeValue::Float(b)) => {
            Ok(ComptimeValue::Float(*a as f64 - b))
        }
        (Sub, ComptimeValue::Float(a), ComptimeValue::Int(b)) => {
            Ok(ComptimeValue::Float(a - *b as f64))
        }
        // ── Mult ─────────────────────────────────────────────────────────────
        (Mult, ComptimeValue::Int(a), ComptimeValue::Int(b)) => a
            .checked_mul(*b)
            .map(ComptimeValue::Int)
            .ok_or_else(|| COMPTIME_INT_OVERFLOW("multiplication")),
        (Mult, ComptimeValue::Float(a), ComptimeValue::Float(b)) => Ok(ComptimeValue::Float(a * b)),
        (Mult, ComptimeValue::Int(a), ComptimeValue::Float(b)) => {
            Ok(ComptimeValue::Float(*a as f64 * b))
        }
        (Mult, ComptimeValue::Float(a), ComptimeValue::Int(b)) => {
            Ok(ComptimeValue::Float(a * *b as f64))
        }
        // ── Div (always produces float, matching Python `/` semantics) ────────
        (Div, ComptimeValue::Int(a), ComptimeValue::Int(b)) => {
            if *b == 0 {
                Err("division by zero in comptime division".to_string())
            } else {
                Ok(ComptimeValue::Float(*a as f64 / *b as f64))
            }
        }
        (Div, ComptimeValue::Float(a), ComptimeValue::Float(b)) => {
            if *b == 0.0 {
                Err("division by zero in comptime division".to_string())
            } else {
                Ok(ComptimeValue::Float(a / b))
            }
        }
        (Div, ComptimeValue::Int(a), ComptimeValue::Float(b)) => {
            if *b == 0.0 {
                Err("division by zero in comptime division".to_string())
            } else {
                Ok(ComptimeValue::Float(*a as f64 / b))
            }
        }
        (Div, ComptimeValue::Float(a), ComptimeValue::Int(b)) => {
            if *b == 0 {
                Err("division by zero in comptime division".to_string())
            } else {
                Ok(ComptimeValue::Float(a / *b as f64))
            }
        }
        // ── FloorDiv (`//`) — Python floors toward negative infinity ─────────
        (FloorDiv, ComptimeValue::Int(a), ComptimeValue::Int(b)) => {
            if *b == 0 {
                Err("integer division or modulo by zero in comptime floor division".to_string())
            } else {
                // Rust's `/` truncates toward zero; Python floors toward -inf.
                // `i64::MIN // -1` is the one quotient that does not fit.
                let q = a
                    .checked_div(*b)
                    .ok_or_else(|| COMPTIME_INT_OVERFLOW("floor division"))?;
                let r = a.checked_rem(*b).unwrap_or(0);
                let q = if (r != 0) && ((r < 0) != (*b < 0)) {
                    q - 1
                } else {
                    q
                };
                Ok(ComptimeValue::Int(q))
            }
        }
        (FloorDiv, _, _)
            if matches!(lhs, ComptimeValue::Int(_) | ComptimeValue::Float(_))
                && matches!(rhs, ComptimeValue::Int(_) | ComptimeValue::Float(_)) =>
        {
            let a = comptime_as_f64(&lhs);
            let b = comptime_as_f64(&rhs);
            if b == 0.0 {
                Err("float floor division by zero in comptime floor division".to_string())
            } else {
                Ok(ComptimeValue::Float(python_float_divmod(a, b).0))
            }
        }
        // ── Mod (`%`) — Python modulo, sign follows the divisor ──────────────
        (Mod, ComptimeValue::Int(a), ComptimeValue::Int(b)) => {
            if *b == 0 {
                Err("integer division or modulo by zero in comptime modulo".to_string())
            } else {
                // Python's `%` result has the same sign as the divisor.
                // (`i64::MIN % -1` overflows the machine remainder; Python
                // says 0.)
                let r = a.checked_rem(*b).unwrap_or(0);
                let r = if (r != 0) && ((r < 0) != (*b < 0)) {
                    r + b
                } else {
                    r
                };
                Ok(ComptimeValue::Int(r))
            }
        }
        (Mod, _, _)
            if matches!(lhs, ComptimeValue::Int(_) | ComptimeValue::Float(_))
                && matches!(rhs, ComptimeValue::Int(_) | ComptimeValue::Float(_)) =>
        {
            let a = comptime_as_f64(&lhs);
            let b = comptime_as_f64(&rhs);
            if b == 0.0 {
                Err("float modulo by zero in comptime modulo".to_string())
            } else {
                // Python `%` for floats: `fmod` corrected so the result takes
                // the divisor's sign (CPython's `float_rem`).
                Ok(ComptimeValue::Float(python_float_divmod(a, b).1))
            }
        }
        // ── Pow (`**`) ───────────────────────────────────────────────────────
        (Pow, ComptimeValue::Int(a), ComptimeValue::Int(b)) => {
            if *b < 0 {
                // Negative exponent → float, matching Python. Use `powf` (not
                // `powi(*b as i32)`, which truncates a very negative exponent).
                Ok(ComptimeValue::Float((*a as f64).powf(*b as f64)))
            } else {
                let exp = u32::try_from(*b)
                    .map_err(|_| "exponent too large in comptime power".to_string())?;
                a.checked_pow(exp)
                    .map(ComptimeValue::Int)
                    .ok_or_else(|| COMPTIME_INT_OVERFLOW("power"))
            }
        }
        (Pow, _, _)
            if matches!(lhs, ComptimeValue::Int(_) | ComptimeValue::Float(_))
                && matches!(rhs, ComptimeValue::Int(_) | ComptimeValue::Float(_)) =>
        {
            let a = comptime_as_f64(&lhs);
            let b = comptime_as_f64(&rhs);
            Ok(ComptimeValue::Float(a.powf(b)))
        }
        _ => Err(format!(
            "operator is not supported between these comptime value types: {:?} {:?} {:?}",
            op, lhs, rhs
        )),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// CPython's `float_divmod`: `(a // b, a % b)` for floats. The remainder is
/// `fmod` corrected to take the divisor's sign; the quotient is derived from
/// that remainder (not from a bare `floor(a / b)`, which is off by one
/// whenever `a / b` rounds up to an integer — `7.0 // 0.1` is `69.0`, not
/// `70.0`), with `-0.0` where CPython produces it.
fn python_float_divmod(vx: f64, wx: f64) -> (f64, f64) {
    let mut modulus = vx % wx;
    let mut div = (vx - modulus) / wx;
    if modulus != 0.0 {
        if (wx < 0.0) != (modulus < 0.0) {
            modulus += wx;
            div -= 1.0;
        }
    } else {
        modulus = 0.0f64.copysign(wx);
    }
    let floordiv = if div != 0.0 {
        let mut fl = div.floor();
        if div - fl > 0.5 {
            fl += 1.0;
        }
        fl
    } else {
        0.0f64.copysign(vx / wx)
    };
    (floordiv, modulus)
}

/// CPython's `repr(float)`: the shortest round-tripping digits, a `.0` on
/// integral values, and scientific notation (`1e+16`, `1e-05`) once the
/// decimal exponent leaves `-4..16`. Mirrors the VM's formatter so `str(x)`
/// folded at comptime equals `str(x)` evaluated at runtime.
pub fn python_float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        };
    }
    let exp10 = x.abs().log10().floor() as i32;
    if !(-4..16).contains(&exp10) {
        let raw = format!("{:e}", x);
        let (mantissa, exp_str) = match raw.split_once('e') {
            Some((m, e)) => (m, e),
            None => return raw,
        };
        let exp: i32 = exp_str.parse().unwrap_or(0);
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{}e{}{:02}", mantissa, sign, exp.abs());
    }
    let s = format!("{}", x);
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s
    } else {
        format!("{}.0", s)
    }
}

/// `str(T)` for a comptime type value: `<class 'int'>` for a bare class,
/// the alias text for a parameterised one (`list[int]`), `None` for `None`.
fn python_type_str(t: &str) -> String {
    if t == "None" || t.contains('[') {
        t.to_owned()
    } else {
        format!("<class '{t}'>")
    }
}

/// `repr` of a comptime value: strings gain Python's quoting (single quotes
/// unless the text contains a single quote and no double quote), everything
/// else prints as `str` does. Containers are rejected like `comptime_str`.
fn comptime_repr(v: &ComptimeValue) -> Result<String, String> {
    match v {
        ComptimeValue::Str(s) => {
            let quote = if s.contains('\'') && !s.contains('"') {
                '"'
            } else {
                '\''
            };
            let mut out = String::with_capacity(s.len() + 2);
            out.push(quote);
            for c in s.chars() {
                match c {
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if c == quote => {
                        out.push('\\');
                        out.push(c);
                    }
                    c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                        out.push_str(&format!("\\x{:02x}", c as u32));
                    }
                    c => out.push(c),
                }
            }
            out.push(quote);
            Ok(out)
        }
        other => comptime_str(other),
    }
}

fn python_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // A raw NUL (or any other control character) in emitted source
            // is a `SyntaxError` on CPython; spell it as an escape.
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn expr_kind_name(expr: &Expr) -> &'static str {
    match expr {
        Expr::BoolOp(_) => "BoolOp",
        Expr::Named(_) => "NamedExpr",
        Expr::BinOp(_) => "BinOp",
        Expr::UnaryOp(_) => "UnaryOp",
        Expr::Lambda(_) => "Lambda",
        Expr::If(_) => "IfExp",
        Expr::Dict(_) => "Dict",
        Expr::Set(_) => "Set",
        Expr::ListComp(_) => "ListComp",
        Expr::SetComp(_) => "SetComp",
        Expr::DictComp(_) => "DictComp",
        Expr::Generator(_) => "GeneratorExp",
        Expr::Await(_) => "Await",
        Expr::Yield(_) => "Yield",
        Expr::YieldFrom(_) => "YieldFrom",
        Expr::Compare(_) => "Compare",
        Expr::Call(_) => "Call",
        Expr::FString(_) => "FString",
        Expr::TString(_) => "TString",
        Expr::StringLiteral(_) => "StringLiteral",
        Expr::BytesLiteral(_) => "BytesLiteral",
        Expr::NumberLiteral(_) => "NumberLiteral",
        Expr::BooleanLiteral(_) => "BooleanLiteral",
        Expr::NoneLiteral(_) => "NoneLiteral",
        Expr::EllipsisLiteral(_) => "EllipsisLiteral",
        Expr::Attribute(_) => "Attribute",
        Expr::Subscript(_) => "Subscript",
        Expr::Starred(_) => "Starred",
        Expr::Name(_) => "Name",
        Expr::List(_) => "List",
        Expr::Tuple(_) => "Tuple",
        Expr::Slice(_) => "Slice",
        Expr::IpyEscapeCommand(_) => "IpyEscapeCommand",
    }
}

/// Human-readable name for a `Stmt` variant. Surfaced in the comptime
/// "unsupported statement" diagnostic so the user sees "while-loop" or
/// "raise" rather than the opaque `Discriminant(...)` debug print.
fn stmt_kind_name(stmt: &Stmt) -> &'static str {
    match stmt {
        Stmt::FunctionDef(_) => "function definition",
        Stmt::ClassDef(_) => "class definition",
        Stmt::Return(_) => "return",
        Stmt::Delete(_) => "del",
        Stmt::Assign(_) => "assignment",
        Stmt::AugAssign(_) => "augmented assignment",
        Stmt::AnnAssign(_) => "annotated assignment",
        Stmt::TypeAlias(_) => "type alias",
        Stmt::For(_) => "for-loop",
        Stmt::While(_) => "while-loop",
        Stmt::If(_) => "if",
        Stmt::With(_) => "with-block",
        Stmt::Match(_) => "match",
        Stmt::Raise(_) => "raise",
        Stmt::Try(_) => "try/except",
        Stmt::Assert(_) => "assert",
        Stmt::Import(_) => "import",
        Stmt::ImportFrom(_) => "from-import",
        Stmt::Global(_) => "global",
        Stmt::Nonlocal(_) => "nonlocal",
        Stmt::Expr(_) => "expression statement",
        Stmt::Pass(_) => "pass",
        Stmt::Break(_) => "break",
        Stmt::Continue(_) => "continue",
        Stmt::IpyEscapeCommand(_) => "IPython escape command",
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};
    use tyc_syntax::preprocess::preprocess;

    /// Process-wide lock for tests that read or mutate `std::env`.
    ///
    /// Cargo runs `#[test]` functions in parallel and `std::env::set_var`
    /// / `remove_var` mutate shared global state — two tests racing on
    /// the same variable will produce flaky failures. Any test that
    /// touches the environment acquires this guard first; tests that
    /// don't touch the env are free to run in parallel as usual.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Acquire [`ENV_LOCK`], recovering from poisoning. A panic inside an
    /// env-touching test would otherwise poison the mutex and make every
    /// subsequent env test fail with `PoisonError` instead of running.
    fn lock_env() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn eval(src: &str) -> (HashMap<String, ComptimeValue>, Diagnostics) {
        let prep = preprocess(src);
        let module = tyc_syntax::parse_module(&prep.python_source)
            .expect("parse failed")
            .into_syntax();
        evaluate_comptime_with_functions(&module, &prep.comptime_bindings, &prep.comptime_functions)
    }

    #[test]
    fn integer_literal_evaluated() {
        let (values, diags) = eval("comptime let PORT: int = 8080\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("PORT"), Some(ComptimeValue::Int(8080))));
    }

    #[test]
    fn runaway_string_concat_hits_the_budget_not_ooms() {
        // A doubling recursion within the depth cap would allocate terabytes;
        // the byte budget must fail the build with a diagnostic instead.
        let src = "\
comptime def grow(s: str, n: int) -> str:
    if n == 0:
        return s
    return grow(s + s, n - 1)

comptime let X: str = grow(\"aaaa\", 40)
";
        let (_values, diags) = eval(src);
        assert!(
            diags.has_errors(),
            "a runaway comptime concatenation must be rejected, not OOM"
        );
    }

    #[test]
    fn modest_string_concat_still_succeeds() {
        // The budget must not reject an ordinary build-time string.
        let (values, diags) = eval("comptime let S: str = \"a\" + \"b\" + \"c\"\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("S"), Some(ComptimeValue::Str(s)) if s == "abc"));
    }

    #[test]
    fn string_literal_evaluated() {
        let (values, diags) = eval("comptime let HOST: str = \"localhost\"\n");
        assert!(!diags.has_errors());
        assert!(matches!(values.get("HOST"), Some(ComptimeValue::Str(s)) if s == "localhost"));
    }

    #[test]
    fn env_with_default_uses_default_when_unset() {
        let _guard = lock_env();
        // Use a unique name that no other test sets.
        std::env::remove_var("__TYPHON_TEST_UNSET_DEFAULT__");
        let (values, diags) = eval(
            "comptime let PORT: int = int(env(\"__TYPHON_TEST_UNSET_DEFAULT__\", \"9000\"))\n",
        );
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("PORT"), Some(ComptimeValue::Int(9000))));
    }

    #[test]
    fn env_with_default_uses_env_when_set() {
        let _guard = lock_env();
        std::env::set_var("__TYPHON_TEST_SET_DEFAULT__", "4321");
        let (values, diags) =
            eval("comptime let PORT: int = int(env(\"__TYPHON_TEST_SET_DEFAULT__\", \"9000\"))\n");
        std::env::remove_var("__TYPHON_TEST_SET_DEFAULT__");
        assert!(!diags.has_errors());
        assert!(matches!(values.get("PORT"), Some(ComptimeValue::Int(4321))));
    }

    #[test]
    fn missing_required_env_is_an_error() {
        let _guard = lock_env();
        std::env::remove_var("__TYPHON_REQUIRED_TEST_UNIQUE__");
        let (_, diags) =
            eval("comptime let DB_URL: str = env(\"__TYPHON_REQUIRED_TEST_UNIQUE__\")\n");
        assert!(diags.has_errors(), "missing env var must be a build error");
    }

    #[test]
    fn int_coercion_on_string() {
        let (values, diags) = eval("comptime let N: int = int(\"42\")\n");
        assert!(!diags.has_errors());
        assert!(matches!(values.get("N"), Some(ComptimeValue::Int(42))));
    }

    // ── Floor-division `//`, modulo `%`, power `**` (arithmetic operators) ────

    #[test]
    fn comptime_int_floor_division() {
        let (values, diags) = eval("comptime let Q: int = 17 // 5\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("Q"), Some(ComptimeValue::Int(3))));
    }

    #[test]
    fn comptime_int_modulo() {
        let (values, diags) = eval("comptime let R: int = 17 % 5\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("R"), Some(ComptimeValue::Int(2))));
    }

    #[test]
    fn comptime_int_power() {
        let (values, diags) = eval("comptime let P: int = 2 ** 10\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("P"), Some(ComptimeValue::Int(1024))));
    }

    #[test]
    fn comptime_int_floor_division_floors_toward_negative_infinity() {
        // Python: -7 // 2 == -4 (floors toward -inf, not toward zero).
        let (values, diags) = eval("comptime let Q: int = -7 // 2\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("Q"), Some(ComptimeValue::Int(-4))));
    }

    #[test]
    fn comptime_int_modulo_sign_follows_divisor() {
        // Python: -7 % 2 == 1 (result sign follows the divisor).
        let (values, diags) = eval("comptime let R: int = -7 % 2\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("R"), Some(ComptimeValue::Int(1))));
        // And 7 % -2 == -1.
        let (values, diags) = eval("comptime let R: int = 7 % -2\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("R"), Some(ComptimeValue::Int(-1))));
    }

    #[test]
    fn comptime_power_with_negative_exponent_is_float() {
        // Python: 2 ** -2 == 0.25 (negative exponent promotes to float).
        let (values, diags) = eval("comptime let F: float = 2 ** -2\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        match values.get("F") {
            Some(ComptimeValue::Float(f)) => assert!((f - 0.25).abs() < 1e-12),
            other => panic!("expected Float(0.25), got {:?}", other),
        }
    }

    #[test]
    fn comptime_power_with_float_exponent_is_float() {
        // 2 ** 0.5 == sqrt(2).
        let (values, diags) = eval("comptime let C: float = 2 ** 0.5\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        match values.get("C") {
            Some(ComptimeValue::Float(f)) => assert!((f - 2.0_f64.sqrt()).abs() < 1e-12),
            other => panic!("expected Float(sqrt 2), got {:?}", other),
        }
    }

    #[test]
    fn comptime_float_floor_division_and_modulo() {
        let (values, diags) = eval("comptime let D: float = 7.0 // 2.0\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        match values.get("D") {
            Some(ComptimeValue::Float(f)) => assert!((f - 3.0).abs() < 1e-12),
            other => panic!("expected Float(3.0), got {:?}", other),
        }
        let (values, diags) = eval("comptime let E: float = 5.5 % 2.0\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        match values.get("E") {
            Some(ComptimeValue::Float(f)) => assert!((f - 1.5).abs() < 1e-12),
            other => panic!("expected Float(1.5), got {:?}", other),
        }
    }

    #[test]
    fn comptime_float_floor_division_matches_cpython_rounding() {
        // `floor(7.0 / 0.1)` is 70.0 because the quotient rounds up to an
        // integer; CPython derives the quotient from the remainder and gets
        // 69.0 (with remainder 0.09999999999999962).
        let (values, diags) = eval("comptime let A: float = 7.0 // 0.1\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(
            matches!(values.get("A"), Some(ComptimeValue::Float(f)) if *f == 69.0),
            "{:?}",
            values.get("A")
        );
        let (values, _) = eval("comptime let E: float = 7.0 % 0.1\n");
        match values.get("E") {
            Some(ComptimeValue::Float(f)) => {
                assert!((f - 0.09999999999999962).abs() < 1e-15, "{f}")
            }
            other => panic!("{other:?}"),
        }
        let (values, _) = eval("comptime let N: float = -7.0 // 2.0\n");
        assert!(matches!(values.get("N"), Some(ComptimeValue::Float(f)) if *f == -4.0));
        let (values, _) = eval("comptime let M: float = -7.5 % 2.0\n");
        assert!(matches!(values.get("M"), Some(ComptimeValue::Float(f)) if *f == 0.5));
        let (values, _) = eval("comptime let Z: float = 4.0 % -2.0\n");
        assert!(
            matches!(values.get("Z"), Some(ComptimeValue::Float(f)) if *f == 0.0 && f.is_sign_negative())
        );
    }

    #[test]
    fn comptime_container_equality_is_structural() {
        for (src, expect) in [
            ("comptime let B: bool = [1, 2] == [1, 2]\n", true),
            ("comptime let B: bool = (1, 2) == (1, 2)\n", true),
            ("comptime let B: bool = [1, 2] == (1, 2)\n", false),
            ("comptime let B: bool = [1, 2] != [1, 3]\n", true),
            ("comptime let B: bool = {\"a\": 1} == {\"a\": 1}\n", true),
            ("comptime let B: bool = {\"a\": 1} == {\"a\": 2}\n", false),
            ("comptime let B: bool = [1, 2.0] == [1.0, 2]\n", true),
        ] {
            let (values, diags) = eval(src);
            assert!(!diags.has_errors(), "{src}: {:?}", diags.errors());
            assert!(
                matches!(values.get("B"), Some(ComptimeValue::Bool(b)) if *b == expect),
                "{src}"
            );
        }
    }

    #[test]
    fn comptime_str_of_floats_and_types_matches_cpython() {
        for (src, expect) in [
            ("comptime let C: str = str(1e16)\n", "1e+16"),
            ("comptime let C: str = str(1e15)\n", "1000000000000000.0"),
            ("comptime let C: str = str(1e-5)\n", "1e-05"),
            ("comptime let C: str = str(0.0001)\n", "0.0001"),
            (
                "comptime let C: str = str(123456789012345678.0)\n",
                "1.2345678901234568e+17",
            ),
            ("comptime let C: str = str(2.0)\n", "2.0"),
            ("comptime let C: str = str(-0.0)\n", "-0.0"),
            ("comptime let C: str = str(int)\n", "<class 'int'>"),
            (
                "comptime let C: str = f\"{1.5e20}|{str}\"\n",
                "1.5e+20|<class 'str'>",
            ),
        ] {
            let (values, diags) = eval(src);
            assert!(!diags.has_errors(), "{src}: {:?}", diags.errors());
            assert!(
                matches!(values.get("C"), Some(ComptimeValue::Str(c)) if c == expect),
                "{src}: {:?}",
                values.get("C")
            );
        }
        assert_eq!(ComptimeValue::Float(1e16).to_python_literal(), "1e+16");
        assert_eq!(
            ComptimeValue::Float(f64::INFINITY).to_python_literal(),
            "float(\"inf\")"
        );
        assert_eq!(
            ComptimeValue::Float(f64::NEG_INFINITY).to_python_literal(),
            "-float(\"inf\")"
        );
        assert_eq!(
            ComptimeValue::Float(f64::NAN).to_python_literal(),
            "float(\"nan\")"
        );
    }

    #[test]
    fn comptime_fstring_debug_specifier_renders_source_and_repr() {
        for (src, expect) in [
            ("comptime let P: str = f\"{1 + 1=}\"\n", "1 + 1=2"),
            ("comptime let P: str = f\"{'a'=}\"\n", "'a'='a'"),
            ("comptime let P: str = f\"{ 2 * 3 = }\"\n", " 2 * 3 = 6"),
            ("comptime let P: str = f\"{'x'=!s}\"\n", "'x'=x"),
        ] {
            let (values, diags) = eval(src);
            assert!(!diags.has_errors(), "{src}: {:?}", diags.errors());
            assert!(
                matches!(values.get("P"), Some(ComptimeValue::Str(p)) if p == expect),
                "{src}: {:?}",
                values.get("P")
            );
        }
    }

    #[test]
    fn comptime_int_overflow_on_negation_and_min_over_minus_one_is_an_error() {
        for src in [
            "comptime let M: int = -(-9223372036854775807 - 1)\n",
            "comptime let M: int = (-9223372036854775807 - 1) // -1\n",
        ] {
            let (_, diags) = eval(src);
            assert!(diags.has_errors(), "{src} must be an overflow error");
        }
        // `i64::MIN % -1` is 0 in Python and must not panic.
        let (values, diags) = eval("comptime let R: int = (-9223372036854775807 - 1) % -1\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("R"), Some(ComptimeValue::Int(0))));
    }

    #[test]
    fn comptime_string_with_control_characters_emits_escapes() {
        let (values, diags) = eval("comptime let R: str = \"a\\x00b\"\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        let v = values.get("R").expect("R");
        assert!(matches!(v, ComptimeValue::Str(s) if s.len() == 3));
        assert_eq!(v.to_python_literal(), "\"a\\x00b\"");
    }

    #[test]
    fn comptime_mixed_int_float_floor_division_promotes() {
        let (values, diags) = eval("comptime let D: float = 7 // 2.0\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        match values.get("D") {
            Some(ComptimeValue::Float(f)) => assert!((f - 3.0).abs() < 1e-12),
            other => panic!("expected Float(3.0), got {:?}", other),
        }
    }

    #[test]
    fn comptime_floor_division_by_zero_is_a_clean_error() {
        // A non-literal zero divisor reaches the evaluator (literal `// 0` is
        // caught earlier by `div_by_zero_literal`). Must be a diagnostic, not
        // a panic.
        let src = "\
comptime def fdiv(a: int, b: int) -> int:
    return a // b

comptime let X: int = fdiv(17, 0)
";
        let (_, diags) = eval(src);
        assert!(
            diags.has_errors(),
            "floor-division by zero must be an error"
        );
    }

    #[test]
    fn comptime_modulo_by_zero_is_a_clean_error() {
        let src = "\
comptime def m(a: int, b: int) -> int:
    return a % b

comptime let X: int = m(17, 0)
";
        let (_, diags) = eval(src);
        assert!(diags.has_errors(), "modulo by zero must be an error");
    }

    #[test]
    fn comptime_power_in_function_body() {
        let src = "\
comptime def calc(n: int) -> int:
    return (n % 3) + (n // 2) + (2 ** n)

comptime let G: int = calc(5)
";
        let (values, diags) = eval(src);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        // (5 % 3) + (5 // 2) + (2 ** 5) == 2 + 2 + 32 == 36
        assert!(matches!(values.get("G"), Some(ComptimeValue::Int(36))));
    }

    #[test]
    fn fstring_with_comptime_values_evaluates() {
        // FINDINGS E7: f-string interpolation in comptime context was
        // rejected with "expression is not a comptime-evaluable
        // constant: FString" even though every interpolated value was
        // itself a comptime constant.
        let src = "\
comptime let APP: str = \"MyApp\"
comptime let MAJOR: int = 2
comptime let MINOR: int = 5
comptime let TITLE: str = f\"{APP} v{MAJOR}.{MINOR}\"
";
        let (values, diags) = eval(src);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(
            matches!(values.get("TITLE"), Some(ComptimeValue::Str(s)) if s == "MyApp v2.5"),
            "TITLE should be \"MyApp v2.5\", got {:?}",
            values.get("TITLE")
        );
    }

    #[test]
    fn fstring_container_interpolation_rejected_in_comptime() {
        // Python's `str(['x'])` -> "['x']" with single-quoted strings,
        // but our literal form uses double quotes. To avoid silently
        // emitting a divergent constant, we reject container values in
        // f-string interpolation outright (codex review on PR #87).
        let src = "comptime let X: str = f\"{['x']}\"\n";
        let (_, diags) = eval(src);
        assert!(
            diags.has_errors(),
            "list interpolation should be rejected at comptime"
        );
    }

    #[test]
    fn fstring_format_spec_rejected_in_comptime() {
        // We do not (yet) emulate Python's f-string format spec at
        // comptime — `f\"{n:>5}\"` must fail explicitly rather than
        // emit a string that disagrees with the runtime form.
        let src = "comptime let X: str = f\"{42:>5}\"\n";
        let (_, diags) = eval(src);
        assert!(
            diags.has_errors(),
            "format spec should be rejected at comptime"
        );
    }

    #[test]
    fn to_python_literal_string() {
        let v = ComptimeValue::Str("hello\nworld".into());
        assert_eq!(v.to_python_literal(), "\"hello\\nworld\"");
    }

    #[test]
    fn to_python_literal_int() {
        assert_eq!(ComptimeValue::Int(42).to_python_literal(), "42");
    }

    #[test]
    fn to_python_literal_bool() {
        assert_eq!(ComptimeValue::Bool(true).to_python_literal(), "True");
        assert_eq!(ComptimeValue::Bool(false).to_python_literal(), "False");
    }

    // ── `comptime def` functions ─────────────────────────────────────────────

    #[test]
    fn comptime_function_called_from_binding_rhs() {
        let src = "\
comptime def double(n: int) -> int:
    return n * 2

comptime let SIZE: int = double(21)
";
        let (values, diags) = eval(src);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("SIZE"), Some(ComptimeValue::Int(42))));
    }

    #[test]
    fn comptime_function_chains_with_env() {
        let _guard = lock_env();
        // `comptime def` can compose with `env()`, demonstrating that the
        // user-defined function and the built-in lookups share a single
        // evaluator scope.
        std::env::set_var("__TYPHON_COMPTIME_CHAIN__", "5");
        let src = "\
comptime def plus_one(n: int) -> int:
    return n + 1

comptime let RESULT: int = plus_one(int(env(\"__TYPHON_COMPTIME_CHAIN__\")))
";
        let (values, diags) = eval(src);
        std::env::remove_var("__TYPHON_COMPTIME_CHAIN__");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("RESULT"), Some(ComptimeValue::Int(6))));
    }

    #[test]
    fn comptime_function_two_args() {
        let src = "\
comptime def join(prefix: str, suffix: str) -> str:
    return prefix + suffix

comptime let URL: str = join(\"https://\", \"example.com\")
";
        let (values, diags) = eval(src);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(
            matches!(values.get("URL"), Some(ComptimeValue::Str(s)) if s == "https://example.com")
        );
    }

    #[test]
    fn comptime_function_can_call_another_comptime_function() {
        let src = "\
comptime def double(n: int) -> int:
    return n * 2

comptime def quad(n: int) -> int:
    return double(double(n))

comptime let X: int = quad(3)
";
        let (values, diags) = eval(src);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("X"), Some(ComptimeValue::Int(12))));
    }

    #[test]
    fn comptime_call_with_wrong_arity_is_an_error() {
        let src = "\
comptime def double(n: int) -> int:
    return n * 2

comptime let X: int = double(1, 2)
";
        let (_, diags) = eval(src);
        assert!(diags.has_errors(), "wrong arity must produce an error");
        let msg = format!("{:?}", diags.errors()[0]);
        assert!(
            msg.contains("expects 1 argument"),
            "expected arity error, got: {msg}"
        );
    }

    #[test]
    fn comptime_call_with_keyword_args_is_an_error() {
        let src = "\
comptime def double(n: int) -> int:
    return n * 2

comptime let X: int = double(n=2)
";
        let (_, diags) = eval(src);
        assert!(diags.has_errors());
        let msg = format!("{:?}", diags.errors()[0]);
        assert!(
            msg.contains("keyword arguments"),
            "expected keyword-arg error, got: {msg}"
        );
    }

    #[test]
    fn comptime_function_with_local_binding_supported() {
        // A local `NAME = EXPR` (or `let NAME: T = EXPR`) inside a
        // comptime function body binds in the local scope and is
        // available to subsequent statements / the return expression.
        let src = "\
comptime def thing() -> int:
    x = 1
    return x + 2

comptime let X: int = thing()
";
        let (values, diags) = eval(src);
        assert!(
            !diags.has_errors(),
            "local binding must be supported: {:?}",
            diags.errors()
        );
        assert!(matches!(values.get("X"), Some(ComptimeValue::Int(3))));
    }

    #[test]
    fn comptime_function_with_annotated_local_binding_supported() {
        let src = "\
comptime def thing() -> int:
    let x: int = 10
    return x * x

comptime let X: int = thing()
";
        let (values, diags) = eval(src);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("X"), Some(ComptimeValue::Int(100))));
    }

    #[test]
    fn comptime_function_if_else_picks_then_branch() {
        let src = "\
comptime def clamp(n: int) -> int:
    if n > 100:
        return 100
    return n

comptime let X: int = clamp(250)
";
        let (values, diags) = eval(src);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("X"), Some(ComptimeValue::Int(100))));
    }

    #[test]
    fn comptime_function_if_else_picks_else_branch() {
        let src = "\
comptime def clamp(n: int) -> int:
    if n > 100:
        return 100
    return n

comptime let X: int = clamp(7)
";
        let (values, diags) = eval(src);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("X"), Some(ComptimeValue::Int(7))));
    }

    #[test]
    fn comptime_function_elif_chain() {
        let src = "\
comptime def grade(score: int) -> str:
    if score >= 90:
        return \"A\"
    elif score >= 80:
        return \"B\"
    elif score >= 70:
        return \"C\"
    else:
        return \"F\"

comptime let G1: str = grade(95)
comptime let G2: str = grade(82)
comptime let G3: str = grade(50)
";
        let (values, diags) = eval(src);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("G1"), Some(ComptimeValue::Str(s)) if s == "A"));
        assert!(matches!(values.get("G2"), Some(ComptimeValue::Str(s)) if s == "B"));
        assert!(matches!(values.get("G3"), Some(ComptimeValue::Str(s)) if s == "F"));
    }

    #[test]
    fn comptime_ternary_if_expression() {
        let src = "\
comptime def sign(n: int) -> int:
    return 1 if n > 0 else -1

comptime let A: int = sign(7)
comptime let B: int = sign(-3)
";
        let (values, diags) = eval(src);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("A"), Some(ComptimeValue::Int(1))));
        assert!(matches!(values.get("B"), Some(ComptimeValue::Int(-1))));
    }

    #[test]
    fn comptime_boolean_operators_short_circuit() {
        let src = "\
comptime def both(a: bool, b: bool) -> bool:
    return a and b

comptime def either(a: bool, b: bool) -> bool:
    return a or b

comptime let T1: bool = both(True, True)
comptime let T2: bool = both(True, False)
comptime let T3: bool = either(False, True)
";
        let (values, diags) = eval(src);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("T1"), Some(ComptimeValue::Bool(true))));
        assert!(matches!(values.get("T2"), Some(ComptimeValue::Bool(false))));
        assert!(matches!(values.get("T3"), Some(ComptimeValue::Bool(true))));
    }

    #[test]
    fn comptime_function_without_return_is_an_error() {
        // A function that runs every branch without hitting `return` is
        // a hard error — the comptime binding has no value to inline.
        let src = "\
comptime def thing(n: int) -> int:
    let x: int = n + 1

comptime let X: int = thing(2)
";
        let (_, diags) = eval(src);
        assert!(diags.has_errors(), "missing return must be an error");
        let msg = format!("{:?}", diags.errors()[0]);
        assert!(msg.contains("fell through"), "got: {msg}");
    }

    #[test]
    fn comptime_function_loop_statement_rejected() {
        // Loops aren't supported in v1 — make sure the error message
        // points at the unsupported statement category rather than
        // silently producing wrong output.
        let src = "\
comptime def thing() -> int:
    for i in [1, 2, 3]:
        let x: int = i
    return 0

comptime let X: int = thing()
";
        let (_, diags) = eval(src);
        assert!(diags.has_errors(), "for-loop body must be rejected");
    }

    #[test]
    fn comptime_function_docstring_before_return_is_allowed() {
        let src = "\
comptime def thing() -> int:
    \"a tiny helper\"
    return 7

comptime let X: int = thing()
";
        let (values, diags) = eval(src);
        assert!(
            !diags.has_errors(),
            "docstring + return must be supported: {:?}",
            diags.errors()
        );
        assert!(matches!(values.get("X"), Some(ComptimeValue::Int(7))));
    }

    #[test]
    fn comptime_function_bare_expression_statement_rejected() {
        // A bare expression statement (other than a leading docstring)
        // must NOT be silently skipped at compile time — the same code
        // emitted as Python `def` would execute the expression, so
        // skipping it would let `1/0` produce a literal value at
        // build time even though the runtime call would crash.
        let src = "\
comptime def thing() -> int:
    1 / 0
    return 1

comptime let X: int = thing()
";
        let (_, diags) = eval(src);
        assert!(
            diags.has_errors(),
            "bare expression statement must be rejected, not silently skipped"
        );
        let msg = format!("{:?}", diags.errors()[0]);
        assert!(
            msg.contains("bare expression statement"),
            "expected dedicated bare-expression diagnostic, got: {msg}"
        );
    }

    #[test]
    fn comptime_function_recursion_depth_capped() {
        // A function that calls itself without a base case must terminate
        // the build with the recursion-depth diagnostic rather than
        // hanging.
        let src = "\
comptime def boom(n: int) -> int:
    return boom(n)

comptime let X: int = boom(1)
";
        let (_, diags) = eval(src);
        assert!(diags.has_errors());
        let msg = format!("{:?}", diags.errors()[0]);
        assert!(msg.contains("depth"), "expected depth error, got: {msg}");
    }

    #[test]
    fn comptime_module_level_name_is_not_in_scope() {
        // Names other than the function's own parameters are not in scope
        // inside a comptime function body — comptime evaluation is
        // hermetic. (A free reference would also be a runtime crash in
        // Python if the bound name didn't exist; failing fast at build
        // time keeps the contract clear.)
        let src = "\
comptime def use_outer() -> int:
    return outer_x

comptime let OUTER_X: int = 1
comptime let Y: int = use_outer()
";
        let (_, diags) = eval(src);
        assert!(diags.has_errors());
        let msg = format!("{:?}", diags.errors()[0]);
        assert!(msg.contains("unknown name"), "got: {msg}");
    }

    // ── #29: comptime container literals + string methods ────────────────

    #[test]
    fn comptime_list_literal_evaluated() {
        let (values, diags) = eval("comptime let TAGS: list[int] = [1, 2, 3]\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        let v = values.get("TAGS").expect("TAGS must evaluate");
        assert!(matches!(v, ComptimeValue::List(xs) if xs.len() == 3));
        assert_eq!(v.to_python_literal(), "[1, 2, 3]");
    }

    #[test]
    fn comptime_tuple_literal_evaluated() {
        let (values, diags) = eval("comptime let PAIR: tuple[int, str] = (1, \"a\")\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert_eq!(
            values.get("PAIR").map(|v| v.to_python_literal()),
            Some("(1, \"a\")".into())
        );
    }

    #[test]
    fn comptime_single_element_tuple_literal_evaluated() {
        // Python's single-element tuple shape; the to_python_literal
        // round-trip must include the trailing comma.
        let (values, diags) = eval("comptime let ONLY: tuple[int] = (1,)\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert_eq!(
            values.get("ONLY").map(|v| v.to_python_literal()),
            Some("(1,)".into())
        );
    }

    #[test]
    fn comptime_dict_literal_evaluated() {
        let (values, diags) = eval("comptime let CFG: dict[str, int] = {\"a\": 1, \"b\": 2}\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        let v = values.get("CFG").expect("CFG must evaluate");
        assert!(matches!(v, ComptimeValue::Dict(items) if items.len() == 2));
        assert_eq!(v.to_python_literal(), "{\"a\": 1, \"b\": 2}");
    }

    #[test]
    fn comptime_empty_containers_evaluated() {
        let (values, _) = eval("comptime let A: list[int] = []\n");
        assert_eq!(
            values.get("A").map(|v| v.to_python_literal()),
            Some("[]".into())
        );
        let (values, _) = eval("comptime let B: dict[str, int] = {}\n");
        assert_eq!(
            values.get("B").map(|v| v.to_python_literal()),
            Some("{}".into())
        );
    }

    #[test]
    fn comptime_nested_containers_evaluated() {
        let (values, diags) = eval("comptime let DATA: list[tuple[int, int]] = [(1, 2), (3, 4)]\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert_eq!(
            values.get("DATA").map(|v| v.to_python_literal()),
            Some("[(1, 2), (3, 4)]".into())
        );
    }

    #[test]
    fn comptime_str_upper_evaluated() {
        let (values, diags) = eval("comptime let SHOUT: str = \"hi\".upper()\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("SHOUT"), Some(ComptimeValue::Str(s)) if s == "HI"));
    }

    #[test]
    fn comptime_str_replace_evaluated() {
        let (values, diags) = eval("comptime let P: str = \"a-b-c\".replace(\"-\", \"_\")\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("P"), Some(ComptimeValue::Str(s)) if s == "a_b_c"));
    }

    #[test]
    fn comptime_str_split_returns_list() {
        let (values, diags) = eval("comptime let PARTS: list[str] = \"a,b,c\".split(\",\")\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert_eq!(
            values.get("PARTS").map(|v| v.to_python_literal()),
            Some("[\"a\", \"b\", \"c\"]".into())
        );
    }

    #[test]
    fn comptime_str_join_returns_string() {
        // Regression for N3 (2026-05-22): the natural pair of `split`
        // was missing from the comptime sandbox.
        let (values, diags) = eval(
            "comptime let TAGS: list[str] = [\"a\", \"b\", \"c\"]\n\
             comptime let CSV: str = \",\".join(TAGS)\n",
        );
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(
            values.get("CSV"),
            Some(ComptimeValue::Str(s)) if s == "a,b,c"
        ));
    }

    #[test]
    fn comptime_str_join_rejects_non_string_element() {
        let (_, diags) = eval("comptime let BAD: str = \",\".join([1, 2, 3])\n");
        assert!(diags.has_errors(), "expected an error for non-str join arg");
    }

    #[test]
    fn comptime_str_startswith_evaluated() {
        let (values, diags) = eval("comptime let IS: bool = \"hello\".startswith(\"he\")\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("IS"), Some(ComptimeValue::Bool(true))));
    }

    #[test]
    fn comptime_str_strip_chains_with_replace() {
        // Composes method calls — receiver of `.replace` is the result
        // of `.strip()`. Tests recursion through the receiver.
        let (values, diags) =
            eval("comptime let X: str = \"  hi  \".strip().replace(\"h\", \"H\")\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("X"), Some(ComptimeValue::Str(s)) if s == "Hi"));
    }

    #[test]
    fn comptime_len_on_str_and_list() {
        let (values, _) = eval("comptime let S: int = len(\"hello\")\n");
        assert!(matches!(values.get("S"), Some(ComptimeValue::Int(5))));
        let (values, _) = eval("comptime let L: int = len([1, 2, 3, 4])\n");
        assert!(matches!(values.get("L"), Some(ComptimeValue::Int(4))));
    }

    // ── #88: comptime subscript ──────────────────────────────────────────

    #[test]
    fn comptime_list_index_evaluates() {
        let (values, diags) = eval("comptime let FIRST: int = [10, 20, 30][0]\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("FIRST"), Some(ComptimeValue::Int(10))));
    }

    #[test]
    fn comptime_list_negative_index_evaluates() {
        let (values, diags) = eval("comptime let LAST: int = [10, 20, 30][-1]\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("LAST"), Some(ComptimeValue::Int(30))));
    }

    #[test]
    fn comptime_tuple_index_evaluates() {
        let (values, diags) = eval("comptime let A: int = (1, 2, 3)[1]\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("A"), Some(ComptimeValue::Int(2))));
    }

    #[test]
    fn comptime_dict_lookup_evaluates() {
        let (values, diags) = eval("comptime let V: int = {\"a\": 1, \"b\": 2}[\"a\"]\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("V"), Some(ComptimeValue::Int(1))));
    }

    #[test]
    fn comptime_str_index_evaluates() {
        let (values, diags) = eval("comptime let C: str = \"hello\"[1]\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("C"), Some(ComptimeValue::Str(s)) if s == "e"));
    }

    #[test]
    fn comptime_list_index_out_of_range_errors() {
        let (_, diags) = eval("comptime let X: int = [1, 2][5]\n");
        assert!(diags.has_errors(), "out-of-range index must error");
        let msg = format!("{}", diags.errors()[0]);
        assert!(msg.contains("out of range"), "got: {msg}");
    }

    #[test]
    fn comptime_dict_missing_key_errors() {
        let (_, diags) = eval("comptime let X: int = {\"a\": 1}[\"missing\"]\n");
        assert!(diags.has_errors(), "missing key must error");
    }

    #[test]
    fn comptime_unsupported_str_method_emits_error() {
        // `str.title()` isn't in the supported set. We want a clear
        // error that lists what IS supported rather than a generic
        // "expression is not comptime-evaluable".
        let (_, diags) = eval("comptime let X: str = \"hi\".title()\n");
        assert!(diags.has_errors(), "title() should not be evaluable");
        let msg = format!("{}", diags.errors()[0]);
        assert!(
            msg.contains("not supported") && msg.contains("title"),
            "expected a friendly diagnostic; got: {msg}"
        );
    }

    #[test]
    fn comptime_user_fn_returning_list_supported() {
        // Closes the loop: a user-defined comptime def can now return
        // a container, since the evaluator and the substitution
        // pipeline both handle them end-to-end.
        let src = "\
comptime def pair(a: int, b: int) -> tuple[int, int]:
    return (a, b)

comptime let P: tuple[int, int] = pair(1, 2)
";
        let (values, diags) = eval(src);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert_eq!(
            values.get("P").map(|v| v.to_python_literal()),
            Some("(1, 2)".into())
        );
    }

    #[test]
    fn comptime_dict_duplicate_keys_last_write_wins() {
        // Matches Python's runtime dict semantics — duplicate keys
        // collapse to the last value. Regression for the
        // gemini-code-assist review on PR #51.
        let (values, diags) = eval("comptime let M: dict[str, int] = {\"a\": 1, \"a\": 2}\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        let v = values.get("M").expect("M must evaluate");
        // Length matches Python's `len({"a": 1, "a": 2}) == 1`.
        assert!(matches!(v, ComptimeValue::Dict(items) if items.len() == 1));
        // The kept value is the last write.
        assert_eq!(v.to_python_literal(), "{\"a\": 2}");
    }

    #[test]
    fn comptime_dict_duplicate_int_keys_dedup_too() {
        // Numeric keys dedup against `values_equal`, matching Python's
        // `{1: "a", 1.0: "b"}` -> `{1: "b"}` semantics.
        let (values, diags) = eval("comptime let M: dict[int, str] = {1: \"a\", 1: \"b\"}\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        let v = values.get("M").expect("M must evaluate");
        assert!(matches!(v, ComptimeValue::Dict(items) if items.len() == 1));
    }

    #[test]
    fn comptime_numeric_constants_match_cpython() {
        // str(float) keeps the `.0`; bool folds into int for == and ordering;
        // two large ints compare exactly (no lossy f64 round-trip).
        let cases = [
            ("comptime let X: str = str(2.0)\n", "X", "\"2.0\""),
            ("comptime let X: str = str(2.5)\n", "X", "\"2.5\""),
            ("comptime let X: bool = True == 1\n", "X", "True"),
            ("comptime let X: bool = True < 2\n", "X", "True"),
            ("comptime let X: bool = 0 == False\n", "X", "True"),
        ];
        for (src, name, expected) in cases {
            let (vals, diags) = eval(src);
            assert!(
                !diags.has_errors(),
                "{src} should fold cleanly: {:?}",
                diags.errors()
            );
            let got = vals.get(name).expect("binding folded").to_python_literal();
            assert_eq!(got, expected, "for `{src}`");
        }
    }

    #[test]
    fn comptime_str_on_container_is_rejected() {
        // Python's `str(["a"])` -> `"['a']"` (single-quoted nested
        // string) doesn't match our double-quoted `to_python_literal`,
        // so reject rather than silently producing a value that would
        // contradict the same expression's runtime result. Regression
        // for the Copilot review on PR #51.
        let (_, diags) = eval("comptime let S: str = str([\"a\"])\n");
        assert!(diags.has_errors(), "str(container) must error");
        let msg = format!("{}", diags.errors()[0]);
        assert!(
            msg.contains("str()") && msg.contains("container"),
            "expected the dedicated diagnostic; got: {msg}"
        );
    }

    #[test]
    fn comptime_method_call_on_list_is_rejected() {
        // `.len()` on a list isn't a Python method — `len()` is the
        // free function instead. Regression for the gemini-code-assist
        // / Copilot reviews on PR #51.
        let (_, diags) = eval("comptime let N: int = [1, 2, 3].len()\n");
        assert!(diags.has_errors(), ".len() on list must error");
        let msg = format!("{}", diags.errors()[0]);
        assert!(
            msg.contains("not supported"),
            "expected the 'not supported' diagnostic; got: {msg}"
        );
    }
}

// ── Purity analysis ───────────────────────────────────────────────────────────

/// Outcome of [`analyse_purity`] for one function.
#[derive(Debug, Clone)]
pub struct PurityFinding {
    /// Function name (for diagnostics).
    pub name: String,
    /// `true` if the user asked the analyser to verify purity (`@pure` / `@memo`
    /// / `@pure(memo=True)`).
    pub declared_pure: bool,
    /// `true` if the user opted into memoisation alongside the purity check
    /// (`@memo`, `@pure(memo=True)`, or the project-wide auto-memoise toggle).
    pub memoise: bool,
    /// Empty when nothing in the body is *provably* impure; otherwise the
    /// first proven violation — I/O, a clock or entropy read, `raise`, a
    /// mutation of module state or of an argument, a read of `mut` module
    /// state, or a call to a helper known to be impure. This is what an
    /// explicit `@pure` / `@memo` reports as `tyc::impure_pure_fn`.
    pub violation: Option<String>,
    /// Empty when every call and read in the body is *provably* pure;
    /// otherwise the first thing the analyser could not prove either way —
    /// a method on a value of unknown type, a call into a module outside the
    /// pure allow-list, a read of a module binding that may be mutated
    /// elsewhere. Never a diagnostic: an explicit `@pure` trusts the author
    /// here, but the silent optimisation paths (`auto-memoise`,
    /// `pgo-memoise`, `auto-parallel`) require [`Self::is_provably_pure`].
    pub unproven: Option<String>,
    /// Set when the declared return type is, or may be, a mutable or lazy
    /// value — `list[...]`, `dict[...]`, an `Iterator`, a non-frozen class,
    /// or no annotation at all. A shared cache would hand every caller the
    /// same object, so the silent memoisation paths never cache such a
    /// function; an explicit `@memo` still does (the author asked for it).
    pub unshareable_return: Option<String>,
    /// Set when a parameter's annotation is not a provably hashable and
    /// immutable type. A cache keyed on a mutable argument returns stale
    /// results once the caller mutates it, so the silent memoisation paths
    /// skip the function; an explicit `@memo` is only refused the
    /// known-unhashable containers (`list` / `dict` / `set` / `bytearray`).
    pub uncacheable_params: Option<String>,
    /// Byte span of the `def` name (for diagnostic placement).
    pub span: (usize, usize),
}

impl PurityFinding {
    /// `true` when the body is provably pure: nothing impure was found *and*
    /// nothing was left unproven.
    pub fn is_provably_pure(&self) -> bool {
        self.violation.is_none() && self.unproven.is_none()
    }

    /// `true` when a silent optimisation (`auto-memoise`, `pgo-memoise`) may
    /// wrap the function in `@functools.cache`: provably pure, hashable and
    /// immutable parameters, and an immutable return value.
    pub fn auto_cacheable(&self) -> bool {
        self.is_provably_pure()
            && self.unshareable_return.is_none()
            && self.uncacheable_params.is_none()
    }

    /// `true` when the optimiser may treat *calls* to the function as
    /// side-effect-free (the `auto-parallel` callee set). An explicit
    /// `@pure` / `@memo` is trusted once nothing provably impure is found;
    /// an inferred candidate must be provably pure.
    pub fn callable_as_pure(&self) -> bool {
        if self.declared_pure {
            self.violation.is_none()
        } else {
            self.is_provably_pure()
        }
    }
}

/// Walk every top-level function in `module` and report on its purity status.
///
/// `auto_memoise` is the value of `[strictness] auto-memoise` from the project
/// `typhon.toml` (defaulting to `false`). When `true`, every pure function is
/// treated as if the user had written `@memo` so the desugarer emits a cache
/// decorator.
///
/// This entry point knows nothing about `class NAME frozen:` markers (the
/// preprocessor strips them before parsing), so every user class counts as
/// mutable for the return / parameter cache-safety checks. The build pipeline
/// uses [`analyse_purity_with`] and passes the frozen class names through.
pub fn analyse_purity(module: &ModModule, auto_memoise: bool) -> Vec<PurityFinding> {
    analyse_purity_with(module, auto_memoise, &HashSet::new())
}

/// [`analyse_purity`] with the set of `frozen` user classes, which count as
/// immutable (and hashable) for the cache-safety checks.
pub fn analyse_purity_with(
    module: &ModModule,
    auto_memoise: bool,
    frozen_classes: &HashSet<String>,
) -> Vec<PurityFinding> {
    let mut out = Vec::new();
    // Phase 1: collect a module-scope view that purity decisions depend
    // on: the module-level bindings and how mutable each one is, the class
    // names (pure constructors) and which of them are immutable, the
    // user-defined functions and whether each is declared pure, and the
    // import aliases so a callee can be resolved to its module path.
    let scope = ModuleScope::collect(&module.body, auto_memoise, frozen_classes);
    analyse_stmts(
        &module.body,
        &scope,
        auto_memoise,
        &mut out,
        /*async_context=*/ false,
    );
    out
}

/// Names of the classes whose `class` keyword sits on one of the marker
/// line starts (the `frozen_class_lines` of the preprocessor, converted to
/// byte offsets with [`tyc_syntax::preprocess::line_byte_starts`]). Mirrors
/// the desugarer's marker matching: a marker covers a class when its offset
/// lies in `[class_start, name_start)`, i.e. on the line of the `class`
/// keyword even when decorators precede it.
pub fn class_names_at_marker_starts(module: &ModModule, starts: &[u32]) -> HashSet<String> {
    let mut out = HashSet::new();
    if starts.is_empty() {
        return out;
    }
    let mut sorted: Vec<u32> = starts.to_vec();
    sorted.sort_unstable();
    for stmt in &module.body {
        if let Stmt::ClassDef(c) = stmt {
            let class_start = c.range.start().to_u32();
            let name_start = c.name.range.start().to_u32();
            let covered = sorted.partition_point(|&off| off < class_start)
                != sorted.partition_point(|&off| off < name_start);
            if covered {
                out.insert(c.name.as_str().to_owned());
            }
        }
    }
    out
}

/// How a module-level binding may change after the module is loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModuleState {
    /// `mut`, rebound, augmented, declared `global` somewhere, or bound by a
    /// module-level loop: reading it makes a result depend on *when* the
    /// call happens.
    Mutable,
    /// A single `let` binding whose declared (or literal) type is immutable:
    /// safe to read.
    Immutable,
    /// A single `let` binding whose value could still be mutated in place
    /// elsewhere (a `list`, a mutable dataclass, an unannotated call result).
    Opaque,
}

/// Snapshot of the module surface that purity decisions need to consult.
#[derive(Debug, Default)]
struct ModuleScope {
    /// Names bound at module level by any assignment form. A pure function
    /// is allowed to *read* these (subject to `bindings`) but not to mutate
    /// them via attribute or subscript writes (`MODULE_LIST.append(x)` /
    /// `MODULE_DICT[k] = v`).
    module_names: Vec<String>,
    /// Mutability classification of each module-level binding.
    bindings: HashMap<String, ModuleState>,
    /// Module-level bindings whose value is provably a builtin type, keyed
    /// to the type head (`str`, `int`, `tuple`, ...). Lets `SEP.join(...)`
    /// on a `let SEP: str` count as a pure builtin method call.
    builtin_typed: HashMap<String, String>,
    /// Class names declared at module level. Class names are treated as
    /// pure callables (the default `@dataclass(slots=True)` emission has
    /// no side effects).
    class_names: Vec<String>,
    /// Module-level classes whose instances are immutable and hashable:
    /// `frozen` classes, enums, `NamedTuple`s, `@dataclass(frozen=True)`.
    immutable_classes: HashSet<String>,
    /// User-defined function name → whether the function is itself declared
    /// pure (`@pure` / `@memo` / `@pure(memo=True)` / auto-memoise). When
    /// a `@pure` function calls another module-defined function, the callee
    /// must also be in this map with `true` to satisfy transitive purity.
    user_functions: HashMap<String, bool>,
    /// Import alias → dotted module path (`import numpy as np` → `np: numpy`,
    /// `from datetime import datetime` → `datetime: datetime.datetime`).
    /// Relative imports map to a `.`-prefixed path that no allow-list
    /// matches, so calls through them are never provably pure.
    imports: HashMap<String, String>,
}

impl ModuleScope {
    fn collect(body: &[Stmt], auto_memoise: bool, frozen_classes: &HashSet<String>) -> Self {
        let mut s = Self::default();
        let shadowed_markers = user_bound_marker_names(body);
        // Classes first: annotation immutability below needs to know them.
        s.immutable_classes = frozen_classes.clone();
        for stmt in body {
            if let Stmt::ClassDef(c) = stmt {
                s.class_names.push(c.name.as_str().to_owned());
                if class_def_is_immutable(c) {
                    s.immutable_classes.insert(c.name.as_str().to_owned());
                }
            }
        }
        // Rebinding evidence anywhere in the module: `global NAME` inside a
        // function, a module-level `+=`, a second assignment, a loop target.
        let mut rebound: HashSet<String> = HashSet::new();
        let mut assigned_once: HashSet<String> = HashSet::new();
        let mut note_binding = |name: &str, rebound: &mut HashSet<String>| {
            if !assigned_once.insert(name.to_owned()) {
                rebound.insert(name.to_owned());
            }
        };
        collect_global_declarations(body, &mut rebound);
        for stmt in body {
            match stmt {
                Stmt::Assign(a) => {
                    for t in &a.targets {
                        for name in bound_names_in_target(t) {
                            note_binding(&name, &mut rebound);
                            if a.mutability == Some(ruff_python_ast::Mutability::Mut) {
                                rebound.insert(name);
                            }
                        }
                    }
                }
                Stmt::AnnAssign(a) => {
                    if let Expr::Name(n) = a.target.as_ref() {
                        note_binding(n.id.as_str(), &mut rebound);
                        if a.mutability == Some(ruff_python_ast::Mutability::Mut) {
                            rebound.insert(n.id.as_str().to_owned());
                        }
                    }
                }
                Stmt::AugAssign(a) => {
                    if let Expr::Name(n) = a.target.as_ref() {
                        rebound.insert(n.id.as_str().to_owned());
                    }
                }
                Stmt::For(f) => {
                    for name in bound_names_in_target(&f.target) {
                        rebound.insert(name);
                    }
                }
                Stmt::With(w) => {
                    for item in &w.items {
                        if let Some(v) = &item.optional_vars {
                            for name in bound_names_in_target(v) {
                                rebound.insert(name);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        for stmt in body {
            match stmt {
                Stmt::Assign(a) => {
                    let value_head = builtin_head_of_value(&a.value);
                    let immutable = value_is_immutable_literal(&a.value);
                    for t in &a.targets {
                        if let Expr::Name(n) = t {
                            let name = n.id.as_str();
                            s.module_names.push(name.to_owned());
                            let state = if rebound.contains(name) {
                                ModuleState::Mutable
                            } else if immutable {
                                ModuleState::Immutable
                            } else {
                                ModuleState::Opaque
                            };
                            s.bindings.insert(name.to_owned(), state);
                            if let Some(head) = value_head {
                                s.builtin_typed.insert(name.to_owned(), head.to_owned());
                            }
                        }
                    }
                }
                Stmt::AnnAssign(a) => {
                    if let Expr::Name(n) = a.target.as_ref() {
                        let name = n.id.as_str();
                        s.module_names.push(name.to_owned());
                        let state = if rebound.contains(name) {
                            ModuleState::Mutable
                        } else if annotation_is_immutable(&a.annotation, &s.immutable_classes) {
                            ModuleState::Immutable
                        } else {
                            ModuleState::Opaque
                        };
                        s.bindings.insert(name.to_owned(), state);
                        if let Some(head) = annotation_builtin_head(&a.annotation) {
                            s.builtin_typed.insert(name.to_owned(), head.to_owned());
                        }
                    }
                }
                Stmt::FunctionDef(f) => {
                    let (declared, _) =
                        decorator_intent(&f.decorator_list, auto_memoise, &shadowed_markers);
                    s.user_functions
                        .insert(f.name.as_str().to_owned(), declared);
                }
                Stmt::Import(i) => {
                    for alias in &i.names {
                        let full = alias.name.as_str();
                        match &alias.asname {
                            Some(asname) => {
                                s.imports
                                    .insert(asname.as_str().to_owned(), full.to_owned());
                            }
                            None => {
                                // `import a.b` binds `a`, which resolves to
                                // the `a` package.
                                let head = full.split('.').next().unwrap_or(full);
                                s.imports.insert(head.to_owned(), head.to_owned());
                            }
                        }
                    }
                }
                Stmt::ImportFrom(i) => {
                    let base = match (&i.module, i.level) {
                        (Some(m), 0) => m.as_str().to_owned(),
                        (Some(m), _) => format!(".{}", m.as_str()),
                        (None, _) => ".".to_owned(),
                    };
                    for alias in &i.names {
                        let name = alias.name.as_str();
                        if name == "*" {
                            continue;
                        }
                        let bound = alias.asname.as_ref().map(|a| a.as_str()).unwrap_or(name);
                        s.imports.insert(bound.to_owned(), format!("{base}.{name}"));
                    }
                }
                _ => {}
            }
        }
        s
    }

    /// Expand the leading import alias of a dotted call path:
    /// `np.sqrt` → `numpy.sqrt`, `datetime.now` (after `from datetime import
    /// datetime`) → `datetime.datetime.now`.
    fn resolve_path(&self, path: &str) -> String {
        let (head, rest) = match path.split_once('.') {
            Some((h, r)) => (h, Some(r)),
            None => (path, None),
        };
        match (self.imports.get(head), rest) {
            (Some(full), Some(r)) => format!("{full}.{r}"),
            (Some(full), None) => full.clone(),
            (None, _) => path.to_owned(),
        }
    }
}

/// Every `global NAME` declaration anywhere in `body` (at any nesting depth).
pub(crate) fn collect_global_declarations(body: &[Stmt], into: &mut HashSet<String>) {
    struct V<'a> {
        into: &'a mut HashSet<String>,
    }
    impl ruff_python_ast::visitor::Visitor<'_> for V<'_> {
        fn visit_stmt(&mut self, s: &Stmt) {
            if let Stmt::Global(g) = s {
                for n in &g.names {
                    self.into.insert(n.as_str().to_owned());
                }
            }
            ruff_python_ast::visitor::walk_stmt(self, s);
        }
    }
    let mut v = V { into };
    for s in body {
        ruff_python_ast::visitor::walk_stmt(&mut v, s);
    }
}

/// Bare names bound by an assignment / loop / `with` target, including
/// tuple and list unpacking and starred elements.
fn bound_names_in_target(target: &Expr) -> Vec<String> {
    let mut out = Vec::new();
    fn go(e: &Expr, out: &mut Vec<String>) {
        match e {
            Expr::Name(n) => out.push(n.id.as_str().to_owned()),
            Expr::Tuple(t) => t.elts.iter().for_each(|e| go(e, out)),
            Expr::List(l) => l.elts.iter().for_each(|e| go(e, out)),
            Expr::Starred(s) => go(&s.value, out),
            _ => {}
        }
    }
    go(target, &mut out);
    out
}

/// A class whose instances are immutable and hashable by construction: an
/// enum, a `NamedTuple`, or an explicit `@dataclass(frozen=True)`. (`class
/// NAME frozen:` is reported by the preprocessor and arrives through
/// [`analyse_purity_with`].)
fn class_def_is_immutable(c: &ruff_python_ast::StmtClassDef) -> bool {
    if let Some(args) = c.arguments.as_deref() {
        for base in &args.args {
            let last = match base {
                Expr::Name(n) => n.id.as_str(),
                Expr::Attribute(a) => a.attr.as_str(),
                Expr::Subscript(s) => match s.value.as_ref() {
                    Expr::Name(n) => n.id.as_str(),
                    Expr::Attribute(a) => a.attr.as_str(),
                    _ => "",
                },
                _ => "",
            };
            if matches!(
                last,
                "Enum" | "IntEnum" | "StrEnum" | "Flag" | "IntFlag" | "ReprEnum" | "NamedTuple"
            ) {
                return true;
            }
        }
    }
    c.decorator_list.iter().any(|d| match &d.expression {
        Expr::Call(call) => {
            let is_dataclass = match call.func.as_ref() {
                Expr::Name(n) => n.id.as_str() == "dataclass",
                Expr::Attribute(a) => a.attr.as_str() == "dataclass",
                _ => false,
            };
            is_dataclass
                && call.arguments.keywords.iter().any(|k| {
                    k.arg.as_ref().is_some_and(|a| a.as_str() == "frozen")
                        && matches!(&k.value, Expr::BooleanLiteral(b) if b.value)
                })
        }
        _ => false,
    })
}

/// Type heads whose values are immutable and hashable.
fn is_immutable_type_head(head: &str) -> bool {
    matches!(
        head,
        "int"
            | "float"
            | "str"
            | "bool"
            | "bytes"
            | "complex"
            | "None"
            | "NoneType"
            | "tuple"
            | "Tuple"
            | "frozenset"
            | "FrozenSet"
            | "range"
            | "Decimal"
            | "Fraction"
            | "datetime"
            | "date"
            | "time"
            | "timedelta"
            | "timezone"
            | "UUID"
            | "PurePath"
            | "PurePosixPath"
            | "PureWindowsPath"
            | "Path"
            | "PosixPath"
            | "WindowsPath"
            | "Pattern"
            | "Enum"
            | "IntEnum"
            | "StrEnum"
            | "Flag"
            | "IntFlag"
            | "Literal"
            | "LiteralString"
            | "Never"
            | "NoReturn"
            | "Callable"
    )
}

/// `true` when values of the annotated type are immutable and hashable, so
/// a cache may both key on them and hand them out to every caller. Unknown
/// heads (user classes that are not frozen, `Any`, `object`, protocols,
/// containers) are `false`.
fn annotation_is_immutable(ann: &Expr, immutable_classes: &HashSet<String>) -> bool {
    match ann {
        Expr::Name(n) => {
            is_immutable_type_head(n.id.as_str()) || immutable_classes.contains(n.id.as_str())
        }
        Expr::Attribute(a) => is_immutable_type_head(a.attr.as_str()),
        Expr::NoneLiteral(_)
        | Expr::EllipsisLiteral(_)
        | Expr::NumberLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::BytesLiteral(_) => true,
        // A quoted forward reference (`"Point"`); the string values inside
        // `Literal[...]` never reach here (that head is accepted whole).
        Expr::StringLiteral(s) => {
            let text = s.value.to_str().trim();
            is_immutable_type_head(text) || immutable_classes.contains(text)
        }
        Expr::BinOp(b) if matches!(b.op, ruff_python_ast::Operator::BitOr) => {
            annotation_is_immutable(&b.left, immutable_classes)
                && annotation_is_immutable(&b.right, immutable_classes)
        }
        Expr::Subscript(s) => {
            let head = match s.value.as_ref() {
                Expr::Name(n) => n.id.as_str(),
                Expr::Attribute(a) => a.attr.as_str(),
                _ => return false,
            };
            let args: Vec<&Expr> = match s.slice.as_ref() {
                Expr::Tuple(t) => t.elts.iter().collect(),
                other => vec![other],
            };
            match head {
                "Literal" => true,
                "Callable" => true,
                "Annotated" => args
                    .first()
                    .is_some_and(|a| annotation_is_immutable(a, immutable_classes)),
                "tuple" | "Tuple" | "frozenset" | "FrozenSet" | "Result" | "Ok" | "Err"
                | "Optional" | "Union" | "type" | "Type" | "Final" | "ClassVar" => args
                    .iter()
                    .all(|a| annotation_is_immutable(a, immutable_classes)),
                _ => false,
            }
        }
        _ => false,
    }
}

/// The builtin type head an annotation names, when it is one of the types
/// whose methods the purity walker knows (`str`, `int`, `tuple`, ...).
fn annotation_builtin_head(ann: &Expr) -> Option<&'static str> {
    let head = match ann {
        Expr::Name(n) => n.id.as_str(),
        Expr::Subscript(s) => match s.value.as_ref() {
            Expr::Name(n) => n.id.as_str(),
            _ => return None,
        },
        _ => return None,
    };
    builtin_type_head(head)
}

fn builtin_type_head(name: &str) -> Option<&'static str> {
    Some(match name {
        "str" => "str",
        "bytes" => "bytes",
        "int" => "int",
        "float" => "float",
        "complex" => "complex",
        "bool" => "bool",
        "tuple" | "Tuple" => "tuple",
        "frozenset" | "FrozenSet" => "frozenset",
        "list" | "List" | "Sequence" | "MutableSequence" => "list",
        "dict" | "Dict" | "Mapping" | "MutableMapping" => "dict",
        "set" | "Set" | "MutableSet" => "set",
        "Result" | "Ok" | "Err" => "Result",
        _ => return None,
    })
}

/// The builtin type head of a value expression, when it is a literal
/// display, an f-string, or a call to a builtin constructor.
fn builtin_head_of_value(value: &Expr) -> Option<&'static str> {
    match value {
        Expr::StringLiteral(_) | Expr::FString(_) => Some("str"),
        Expr::BytesLiteral(_) => Some("bytes"),
        Expr::NumberLiteral(n) => Some(match n.value {
            Number::Int(_) => "int",
            Number::Float(_) => "float",
            Number::Complex { .. } => "complex",
        }),
        Expr::BooleanLiteral(_) => Some("bool"),
        Expr::Tuple(_) => Some("tuple"),
        Expr::List(_) | Expr::ListComp(_) => Some("list"),
        Expr::Dict(_) | Expr::DictComp(_) => Some("dict"),
        Expr::Set(_) | Expr::SetComp(_) => Some("set"),
        Expr::Call(c) => match c.func.as_ref() {
            Expr::Name(n) => match n.id.as_str() {
                "sorted" => Some("list"),
                "Ok" | "Err" => Some("Result"),
                other => builtin_type_head(other),
            },
            _ => None,
        },
        _ => None,
    }
}

/// `true` when a value expression is an immutable constant: a scalar or
/// string literal, a tuple of such, or arithmetic over them.
fn value_is_immutable_literal(value: &Expr) -> bool {
    match value {
        Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::NumberLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NoneLiteral(_)
        | Expr::EllipsisLiteral(_) => true,
        Expr::Tuple(t) => t.elts.iter().all(value_is_immutable_literal),
        Expr::BinOp(b) => {
            value_is_immutable_literal(&b.left) && value_is_immutable_literal(&b.right)
        }
        Expr::UnaryOp(u) => value_is_immutable_literal(&u.operand),
        Expr::Call(c) => {
            matches!(c.func.as_ref(), Expr::Name(n) if matches!(n.id.as_str(), "frozenset" | "tuple" | "int" | "float" | "str" | "bytes" | "bool"))
                && c.arguments.args.iter().all(value_is_immutable_literal)
                && c.arguments.keywords.is_empty()
        }
        _ => false,
    }
}

fn analyse_stmts(
    body: &[Stmt],
    module: &ModuleScope,
    auto_memoise: bool,
    out: &mut Vec<PurityFinding>,
    _async_context: bool,
) {
    // Only the OUTER call (the recursion entry from `analyse_purity`)
    // visits top-level functions, which is the only scope the desugarer
    // can rewrite by name. Recursing into function/class bodies would
    // collect nested `@memo def f` findings that, if they shared a name
    // with a top-level `def f`, would cause the desugarer to inject
    // `@functools.cache` on the wrong function. Restrict the analyser to
    // top-level scope until we thread span-based identifiers through to
    // the desugarer.
    let shadowed_markers = user_bound_marker_names(body);
    for stmt in body {
        if let Stmt::FunctionDef(f) = stmt {
            let (declared, memo) =
                decorator_intent(&f.decorator_list, auto_memoise, &shadowed_markers);
            // Run the purity check whenever the user opted in OR the
            // project asked for automatic caching. Auto-memoise never
            // produces an error (see `purity_diagnostics`); it just
            // gates cache-decorator injection on the function silently
            // passing.
            if declared || memo {
                let verdict = check_purity(f, module);
                out.push(PurityFinding {
                    name: f.name.as_str().to_owned(),
                    declared_pure: declared,
                    memoise: memo,
                    violation: verdict.violation,
                    unproven: verdict.unproven,
                    unshareable_return: verdict.unshareable_return,
                    uncacheable_params: verdict.uncacheable_params,
                    span: (
                        f.range.start().to_usize(),
                        f.range.start().to_usize() + f.name.as_str().len(),
                    ),
                });
            }
        }
    }
}

/// Inspect a decorator list. Returns `(declared_pure, memoise)`:
///   - `declared_pure` is `true` if `@pure`, `@pure(...)`, or `@memo` appears
///     — i.e. the user explicitly opted in to having the analyser enforce
///     purity. `auto_memoise` does **not** flip this flag; it only opts the
///     project into automatic caching of already-passable functions, not
///     into hard purity errors for ordinary impure code.
///   - `memoise` is `true` if the user asked for caching: `@memo`,
///     `@pure(memo=True)`, or `auto_memoise`. The desugarer only injects
///     `@functools.cache` when the function ALSO passes the purity check;
///     `auto_memoise` is therefore a silent best-effort, never a hard error.
fn decorator_intent(
    decorators: &[Decorator],
    auto_memoise: bool,
    shadowed: &std::collections::HashSet<String>,
) -> (bool, bool) {
    let mut declared = false;
    let mut memoise = auto_memoise;
    for d in decorators {
        // A `pure` / `memo` this module defines or imports for itself is the
        // user's decorator, not Typhon's marker. Reading it as the marker made
        // `@functools.cache` get injected on top of an unrelated third-party
        // decorator.
        if let Some(name) = decorator_head_name(&d.expression) {
            if shadowed.contains(&name) {
                continue;
            }
        }
        match &d.expression {
            Expr::Name(n) if n.id.as_str() == "pure" => {
                declared = true;
            }
            Expr::Name(n) if n.id.as_str() == "memo" => {
                declared = true;
                memoise = true;
            }
            Expr::Call(call) => match call.func.as_ref() {
                Expr::Name(n) if n.id.as_str() == "pure" => {
                    declared = true;
                    if call.arguments.keywords.iter().any(|k| {
                        k.arg.as_ref().is_some_and(|a| a.as_str() == "memo")
                            && matches!(
                                &k.value,
                                Expr::BooleanLiteral(b) if b.value
                            )
                    }) {
                        memoise = true;
                    }
                }
                Expr::Name(n) if n.id.as_str() == "memo" => {
                    declared = true;
                    memoise = true;
                }
                _ => {}
            },
            _ => {}
        }
    }
    (declared, memoise)
}

/// The bare name a decorator expression applies, for `@name` and `@name(...)`.
fn decorator_head_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name(n) => Some(n.id.as_str().to_owned()),
        Expr::Call(c) => decorator_head_name(&c.func),
        _ => None,
    }
}

/// Names among `pure` / `memo` / `gatherable` that this module defines or
/// imports for itself. Re-exported from `tyc-syntax`, the single shared
/// derivation — the analyser (which decides whether a decorator is Typhon's
/// marker) and `tyc-desugar` (which decides whether to strip or replace it)
/// must agree, so both consume the same function rather than keeping copies
/// that can drift.
pub use tyc_syntax::user_bound_marker_names;

/// The four channels a purity check reports on (see [`PurityFinding`]).
#[derive(Debug, Default)]
struct PurityVerdict {
    violation: Option<String>,
    unproven: Option<String>,
    unshareable_return: Option<String>,
    uncacheable_params: Option<String>,
}

fn check_purity(f: &ruff_python_ast::StmtFunctionDef, module: &ModuleScope) -> PurityVerdict {
    let parameters: &Parameters = &f.parameters;
    let body: &[Stmt] = &f.body;
    let mut verdict = PurityVerdict::default();
    // 1. Synchronous.
    if f.is_async {
        verdict.violation = Some("function is async — pure functions must be synchronous".into());
        return verdict;
    }
    // 2. Hashable parameter types — memoisation backs every cache hit with a
    //    dict keyed on `args`. Reject obviously unhashable annotations so the
    //    cache decorator the desugarer injects never crashes at runtime.
    if let Some(reason) = unhashable_param_reason(parameters) {
        verdict.violation = Some(reason);
        return verdict;
    }
    // Cache-safety of the signature (silent memoisation paths only): every
    // parameter provably hashable *and* immutable, the return value
    // immutable. Neither is a purity violation.
    verdict.uncacheable_params = uncacheable_param_reason(parameters, module);
    verdict.unshareable_return = match f.returns.as_deref() {
        None => Some(format!(
            "`{}` has no return annotation, so its result cannot be proven safe to share from a cache",
            f.name.as_str()
        )),
        Some(ann) if annotation_is_immutable(ann, &module.immutable_classes) => None,
        Some(ann) => Some(format!(
            "`{}` returns `{}`, a mutable or lazy value that a shared cache would alias between callers",
            f.name.as_str(),
            annotation_text(ann)
        )),
    };
    // The remaining conditions are checked by walking the body.
    let bindings = collect_local_bindings(body, module);
    let mut ctx = PurityCtx {
        violation: None,
        unproven: None,
        module,
        params: parameter_names(parameters),
        locals: bindings.names,
        builtin_typed: bindings.types,
        fresh: bindings.fresh,
    };
    for pwd in parameters
        .posonlyargs
        .iter()
        .chain(parameters.args.iter())
        .chain(parameters.kwonlyargs.iter())
    {
        if let Some(head) = pwd
            .parameter
            .annotation
            .as_deref()
            .and_then(annotation_builtin_head)
        {
            ctx.builtin_typed
                .insert(pwd.parameter.name.as_str().to_owned(), head.to_owned());
        }
    }
    walk_stmts_purity(body, &mut ctx);
    verdict.violation = ctx.violation;
    verdict.unproven = ctx.unproven;
    verdict
}

/// Rough source text of an annotation for messages.
fn annotation_text(ann: &Expr) -> String {
    match ann {
        Expr::Name(n) => n.id.as_str().to_owned(),
        Expr::Attribute(a) => format!("{}.{}", annotation_text(&a.value), a.attr.as_str()),
        Expr::Subscript(s) => {
            let inner = match s.slice.as_ref() {
                Expr::Tuple(t) => t
                    .elts
                    .iter()
                    .map(annotation_text)
                    .collect::<Vec<_>>()
                    .join(", "),
                other => annotation_text(other),
            };
            format!("{}[{}]", annotation_text(&s.value), inner)
        }
        Expr::BinOp(b) => format!(
            "{} | {}",
            annotation_text(&b.left),
            annotation_text(&b.right)
        ),
        Expr::NoneLiteral(_) => "None".to_owned(),
        Expr::StringLiteral(s) => s.value.to_str().to_owned(),
        Expr::EllipsisLiteral(_) => "...".to_owned(),
        _ => "…".to_owned(),
    }
}

fn parameter_names(parameters: &Parameters) -> HashSet<String> {
    let mut out: HashSet<String> = parameters
        .posonlyargs
        .iter()
        .chain(parameters.args.iter())
        .chain(parameters.kwonlyargs.iter())
        .map(|p| p.parameter.name.as_str().to_owned())
        .collect();
    if let Some(v) = &parameters.vararg {
        out.insert(v.name.as_str().to_owned());
    }
    if let Some(k) = &parameters.kwarg {
        out.insert(k.name.as_str().to_owned());
    }
    out
}

/// If `parameters` declares a parameter whose annotation is a known-unhashable
/// container type (`list`, `dict`, `set`, `bytearray`), return a reason
/// string. Annotations the analyser doesn't understand are treated as
/// hashable by default — we err on the side of accepting code rather than
/// blocking valid use of opaque types.
fn unhashable_param_reason(parameters: &Parameters) -> Option<String> {
    let report = |name: &str, ty: &str| -> Option<String> {
        Some(format!(
            "parameter '{}' has unhashable type `{}` — pure functions need hashable args so memoised caches can key on them",
            name, ty
        ))
    };
    for pwd in parameters
        .posonlyargs
        .iter()
        .chain(parameters.args.iter())
        .chain(parameters.kwonlyargs.iter())
    {
        if let Some(ann) = &pwd.parameter.annotation {
            if let Some(t) = annotation_unhashable_name(ann) {
                return report(pwd.parameter.name.as_str(), &t);
            }
        }
    }
    None
}

/// The stricter, cache-safety version of [`unhashable_param_reason`] used by
/// the silent memoisation paths: every parameter must be annotated with a
/// provably immutable (hence hashable) type, and `*args` / `**kwargs` are
/// out (a `**kwargs` dict never hashes; `*args` may carry anything).
fn uncacheable_param_reason(parameters: &Parameters, module: &ModuleScope) -> Option<String> {
    if let Some(v) = &parameters.vararg {
        return Some(format!(
            "`*{}` may carry unhashable or mutable values",
            v.name.as_str()
        ));
    }
    if let Some(k) = &parameters.kwarg {
        return Some(format!("`**{}` is never hashable", k.name.as_str()));
    }
    for pwd in parameters
        .posonlyargs
        .iter()
        .chain(parameters.args.iter())
        .chain(parameters.kwonlyargs.iter())
    {
        let name = pwd.parameter.name.as_str();
        match pwd.parameter.annotation.as_deref() {
            None => {
                return Some(format!(
                    "parameter `{name}` has no annotation, so it cannot be proven hashable and immutable"
                ))
            }
            Some(ann) if annotation_is_immutable(ann, &module.immutable_classes) => {}
            Some(ann) => {
                return Some(format!(
                    "parameter `{name}: {}` is not provably hashable and immutable — a cache keyed on it would go stale if the caller mutates it",
                    annotation_text(ann)
                ))
            }
        }
    }
    None
}

fn annotation_unhashable_name(ann: &Expr) -> Option<String> {
    // Both `list` and `list[int]` are unhashable. Look at the head identifier.
    let head = match ann {
        Expr::Name(n) => n.id.as_str().to_owned(),
        Expr::Subscript(s) => match s.value.as_ref() {
            Expr::Name(n) => n.id.as_str().to_owned(),
            _ => return None,
        },
        _ => return None,
    };
    matches!(
        head.as_str(),
        "list" | "List" | "dict" | "Dict" | "set" | "Set" | "bytearray"
    )
    .then_some(head)
}

/// What [`collect_local_bindings`] learns about a function body.
#[derive(Default)]
struct LocalBindings {
    /// Every name bound in this scope.
    names: HashSet<String>,
    /// Locals whose annotation or initialiser fixes a builtin type head.
    types: HashMap<String, String>,
    /// Locals whose every initialiser creates a fresh object.
    fresh: HashSet<String>,
}

/// Names bound in a function body (this scope only — nested `def` / `class`
/// bodies are their own scopes, though their *names* bind here), plus the
/// builtin type head of every local whose annotation or initialiser makes it
/// obvious (`let s: str = ...`, `n = 0`, `xs = sorted(...)`), plus the locals
/// that are provably fresh objects.
fn collect_local_bindings(body: &[Stmt], module: &ModuleScope) -> LocalBindings {
    use ruff_python_ast::visitor::source_order::{
        walk_expr, walk_pattern, walk_stmt, SourceOrderVisitor,
    };
    use ruff_python_ast::Pattern;

    #[derive(Default)]
    struct V<'m> {
        names: HashSet<String>,
        types: HashMap<String, String>,
        fresh_candidates: HashSet<String>,
        not_fresh: HashSet<String>,
        class_names: &'m [String],
    }
    impl<'ast> SourceOrderVisitor<'ast> for V<'_> {
        fn visit_stmt(&mut self, s: &'ast Stmt) {
            match s {
                Stmt::FunctionDef(f) => {
                    self.names.insert(f.name.as_str().to_owned());
                }
                Stmt::ClassDef(c) => {
                    self.names.insert(c.name.as_str().to_owned());
                }
                Stmt::Import(i) => {
                    for alias in &i.names {
                        let bound = match &alias.asname {
                            Some(asname) => asname.as_str(),
                            None => alias.name.as_str().split('.').next().unwrap_or(""),
                        };
                        self.names.insert(bound.to_owned());
                    }
                }
                Stmt::ImportFrom(i) => {
                    for alias in &i.names {
                        let bound = match &alias.asname {
                            Some(asname) => asname.as_str(),
                            None => alias.name.as_str(),
                        };
                        self.names.insert(bound.to_owned());
                    }
                }
                Stmt::Assign(a) => {
                    if let [Expr::Name(n)] = a.targets.as_slice() {
                        let name = n.id.as_str().to_owned();
                        if let Some(head) = builtin_head_of_value(&a.value) {
                            self.types.insert(name.clone(), head.to_owned());
                        }
                        if value_is_fresh(&a.value, self.class_names) {
                            self.fresh_candidates.insert(name.clone());
                        } else {
                            self.not_fresh.insert(name.clone());
                        }
                        self.names.insert(name);
                        // Only the value is walked: the target is accounted
                        // for above (walking it would mark it not-fresh).
                        self.visit_expr(&a.value);
                        return;
                    }
                    walk_stmt(self, s);
                }
                Stmt::AnnAssign(a) => {
                    if let Expr::Name(n) = a.target.as_ref() {
                        let name = n.id.as_str().to_owned();
                        self.names.insert(name.clone());
                        if let Some(head) = annotation_builtin_head(&a.annotation) {
                            self.types.insert(name.clone(), head.to_owned());
                        }
                        if let Some(v) = a.value.as_deref() {
                            if value_is_fresh(v, self.class_names) {
                                self.fresh_candidates.insert(name);
                            } else {
                                self.not_fresh.insert(name);
                            }
                            self.visit_expr(v);
                        }
                        return;
                    }
                    walk_stmt(self, s);
                }
                Stmt::Try(t) => {
                    for h in &t.handlers {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                        if let Some(alias) = &h.name {
                            self.names.insert(alias.id.as_str().to_owned());
                        }
                    }
                    walk_stmt(self, s);
                }
                _ => walk_stmt(self, s),
            }
        }

        fn visit_expr(&mut self, e: &'ast Expr) {
            if let Expr::Name(n) = e {
                if !n.ctx.is_load() {
                    // Loop / `with` / unpacking / walrus targets: bound
                    // here, but to an object of unknown provenance.
                    self.names.insert(n.id.as_str().to_owned());
                    self.not_fresh.insert(n.id.as_str().to_owned());
                }
            }
            walk_expr(self, e);
        }

        fn visit_pattern(&mut self, p: &'ast Pattern) {
            let captured = match p {
                Pattern::MatchAs(m) => m.name.as_ref(),
                Pattern::MatchStar(m) => m.name.as_ref(),
                Pattern::MatchMapping(m) => m.rest.as_ref(),
                _ => None,
            };
            if let Some(id) = captured {
                self.names.insert(id.as_str().to_owned());
            }
            walk_pattern(self, p);
        }
    }

    let mut v = V {
        class_names: &module.class_names,
        ..Default::default()
    };
    for stmt in body {
        v.visit_stmt(stmt);
    }
    let fresh = v
        .fresh_candidates
        .difference(&v.not_fresh)
        .cloned()
        .collect();
    LocalBindings {
        names: v.names,
        types: v.types,
        fresh,
    }
}

/// `true` when evaluating `value` necessarily creates a new object: a
/// container display or comprehension, a builtin container constructor, or
/// a call to one of this module's classes.
fn value_is_fresh(value: &Expr, class_names: &[String]) -> bool {
    match value {
        Expr::List(_)
        | Expr::ListComp(_)
        | Expr::Dict(_)
        | Expr::DictComp(_)
        | Expr::Set(_)
        | Expr::SetComp(_) => true,
        Expr::Call(c) => match c.func.as_ref() {
            Expr::Name(n) => {
                matches!(
                    n.id.as_str(),
                    "list" | "dict" | "set" | "sorted" | "bytearray" | "reversed"
                ) || class_names.iter().any(|c| c == n.id.as_str())
            }
            _ => false,
        },
        _ => false,
    }
}

struct PurityCtx<'a> {
    violation: Option<String>,
    unproven: Option<String>,
    module: &'a ModuleScope,
    /// The function's parameter names.
    params: HashSet<String>,
    /// Names bound in the function body (plus comprehension / lambda
    /// variables as the walk enters them).
    locals: HashSet<String>,
    /// Parameters and locals provably of a builtin type: name → type head.
    builtin_typed: HashMap<String, String>,
    /// Locals whose every initialiser creates a fresh object (a display,
    /// comprehension, builtin constructor or module class constructor):
    /// mutating them in place is unobservable outside the function.
    fresh: HashSet<String>,
}

impl PurityCtx<'_> {
    fn fail(&mut self, reason: impl Into<String>) {
        if self.violation.is_none() {
            self.violation = Some(reason.into());
        }
    }

    fn unproven(&mut self, reason: impl Into<String>) {
        if self.unproven.is_none() {
            self.unproven = Some(reason.into());
        }
    }

    fn is_param(&self, name: &str) -> bool {
        self.params.contains(name)
    }

    fn is_local(&self, name: &str) -> bool {
        self.locals.contains(name)
    }

    /// The builtin type head of a parameter, local or module binding, when
    /// it is provable from an annotation or a literal initialiser.
    fn builtin_type_of(&self, name: &str) -> Option<&str> {
        if self.is_param(name) || self.is_local(name) {
            return self.builtin_typed.get(name).map(String::as_str);
        }
        self.module.builtin_typed.get(name).map(String::as_str)
    }
}

fn walk_stmts_purity(stmts: &[Stmt], ctx: &mut PurityCtx) {
    for stmt in stmts {
        if ctx.violation.is_some() {
            return;
        }
        walk_stmt_purity(stmt, ctx);
    }
}

fn walk_stmt_purity(stmt: &Stmt, ctx: &mut PurityCtx) {
    match stmt {
        Stmt::Raise(_) => {
            ctx.fail("pure functions must not raise — return Result[T, E] to express failure")
        }
        // `global` would let a function alias a module name and write to it
        // without going through an attribute. Forbid it outright. With `global`
        // gone, every bare-name assignment inside a pure body is local by
        // Python's own scoping rules — so we no longer flag those.
        Stmt::Global(_) => ctx.fail("pure functions must not declare `global`"),
        Stmt::Nonlocal(_) => ctx.fail("pure functions must not declare `nonlocal`"),
        Stmt::Try(t) => {
            // try / except is allowed only if it doesn't catch and re-raise;
            // we conservatively reject any try block since it implies exception
            // handling, which is a non-Result error channel.
            ctx.fail("pure functions must not use `try`/`except` — handle errors with Result");
            let _ = t;
        }
        Stmt::Assign(a) => {
            for t in &a.targets {
                check_mutation_target(t, ctx);
            }
            walk_expr_purity(&a.value, ctx);
        }
        Stmt::AnnAssign(a) => {
            check_mutation_target(&a.target, ctx);
            if let Some(v) = &a.value {
                walk_expr_purity(v, ctx);
            }
        }
        Stmt::AugAssign(a) => {
            // `x += 1` on a bare local name is fine. `MODULE_LIST += [x]` or
            // `obj.attr += 1` mutate referenced state — flag those.
            check_mutation_target(&a.target, ctx);
            walk_expr_purity(&a.value, ctx);
        }
        Stmt::Delete(d) => {
            for t in &d.targets {
                check_mutation_target(t, ctx);
            }
        }
        Stmt::Return(r) => {
            if let Some(v) = &r.value {
                walk_expr_purity(v, ctx);
            }
        }
        Stmt::Expr(e) => walk_expr_purity(&e.value, ctx),
        Stmt::If(s) => {
            walk_expr_purity(&s.test, ctx);
            walk_stmts_purity(&s.body, ctx);
            for clause in &s.elif_else_clauses {
                if let Some(test) = &clause.test {
                    walk_expr_purity(test, ctx);
                }
                walk_stmts_purity(&clause.body, ctx);
            }
        }
        Stmt::While(s) => {
            walk_expr_purity(&s.test, ctx);
            walk_stmts_purity(&s.body, ctx);
            walk_stmts_purity(&s.orelse, ctx);
        }
        Stmt::For(s) => {
            if s.is_async {
                ctx.fail("pure functions must not use async constructs");
                return;
            }
            walk_expr_purity(&s.iter, ctx);
            walk_stmts_purity(&s.body, ctx);
            walk_stmts_purity(&s.orelse, ctx);
        }
        Stmt::With(s) => {
            if s.is_async {
                ctx.fail("pure functions must not use async constructs");
                return;
            }
            // A context manager runs `__enter__` / `__exit__` — code the
            // walker cannot see. `open(...)` is caught as an I/O call; any
            // other manager is not provably pure.
            for item in &s.items {
                walk_expr_purity(&item.context_expr, ctx);
            }
            ctx.unproven("uses a `with` block — the context manager's `__enter__` / `__exit__` are not provably pure");
            walk_stmts_purity(&s.body, ctx);
        }
        Stmt::Match(s) => {
            walk_expr_purity(&s.subject, ctx);
            for case in &s.cases {
                if let Some(g) = &case.guard {
                    walk_expr_purity(g, ctx);
                }
                walk_stmts_purity(&case.body, ctx);
            }
        }
        Stmt::FunctionDef(_) | Stmt::ClassDef(_) => {
            // Nested defs / classes are out of scope for purity propagation.
        }
        _ => {}
    }
}

fn walk_expr_purity(expr: &Expr, ctx: &mut PurityCtx) {
    match expr {
        Expr::Await(_) | Expr::Yield(_) | Expr::YieldFrom(_) => {
            ctx.fail("pure functions must not be async or generator-flavoured (`await`, `yield`)")
        }
        Expr::Call(c) => {
            match classify_call(c, ctx) {
                CallVerdict::Impure(reason) => {
                    ctx.fail(reason);
                    return;
                }
                CallVerdict::Unproven(reason) => ctx.unproven(reason),
                CallVerdict::Pure => {}
            }
            // The receiver of a method call is walked for the reads it
            // performs; the callee name itself was classified above. A
            // receiver that is a module path (`json.dumps`) was resolved as
            // part of the callee and is not a value read.
            if let Expr::Attribute(a) = c.func.as_ref() {
                let rooted_at_import = dotted_path(&a.value).is_some_and(|base| {
                    let head = base.split('.').next().unwrap_or("");
                    ctx.module.imports.contains_key(head)
                        && !ctx.is_param(head)
                        && !ctx.is_local(head)
                });
                if !rooted_at_import {
                    walk_expr_purity(&a.value, ctx);
                }
            }
            for a in c.arguments.args.iter() {
                walk_expr_purity(a, ctx);
            }
            for k in c.arguments.keywords.iter() {
                walk_expr_purity(&k.value, ctx);
            }
        }
        Expr::BinOp(b) => {
            walk_expr_purity(&b.left, ctx);
            walk_expr_purity(&b.right, ctx);
        }
        Expr::BoolOp(b) => {
            for v in &b.values {
                walk_expr_purity(v, ctx);
            }
        }
        Expr::UnaryOp(u) => walk_expr_purity(&u.operand, ctx),
        Expr::Compare(c) => {
            walk_expr_purity(&c.left, ctx);
            for cm in c.comparators.iter() {
                walk_expr_purity(cm, ctx);
            }
        }
        Expr::If(i) => {
            walk_expr_purity(&i.test, ctx);
            walk_expr_purity(&i.body, ctx);
            walk_expr_purity(&i.orelse, ctx);
        }
        Expr::Tuple(t) => {
            for e in &t.elts {
                walk_expr_purity(e, ctx);
            }
        }
        Expr::List(l) => {
            for e in &l.elts {
                walk_expr_purity(e, ctx);
            }
        }
        Expr::Attribute(a) => walk_expr_purity(&a.value, ctx),
        Expr::Subscript(s) => {
            walk_expr_purity(&s.value, ctx);
            walk_expr_purity(&s.slice, ctx);
        }
        // The arms below all used to fall into `_ => {}`, which meant the
        // purity check simply did not look inside them — so `@pure` accepted a
        // function whose only impurity was a `random()` call in a
        // comprehension, an f-string, a lambda body, or a dict/set literal.
        // With `@memo` (or `auto-memoise`) that put `@functools.cache` on a
        // *nondeterministic* function, which is a wrong answer that gets more
        // wrong the longer the process runs.
        //
        // R5: no `_ => {}` in an analysis visitor. Every remaining arm is
        // enumerated so adding a node type to the AST forces a decision here
        // instead of silently widening what `@pure` accepts.
        Expr::Set(x) => {
            for e in &x.elts {
                walk_expr_purity(e, ctx);
            }
        }
        Expr::ListComp(x) => {
            bind_comprehension_targets(&x.generators, ctx);
            walk_expr_purity(&x.elt, ctx);
            walk_comprehension_clauses(&x.generators, ctx);
        }
        Expr::SetComp(x) => {
            bind_comprehension_targets(&x.generators, ctx);
            walk_expr_purity(&x.elt, ctx);
            walk_comprehension_clauses(&x.generators, ctx);
        }
        Expr::Generator(x) => {
            bind_comprehension_targets(&x.generators, ctx);
            walk_expr_purity(&x.elt, ctx);
            walk_comprehension_clauses(&x.generators, ctx);
        }
        Expr::DictComp(x) => {
            bind_comprehension_targets(&x.generators, ctx);
            // The vendored fork models the key as optional (it is absent for
            // a `**spread` entry in the equivalent display form).
            if let Some(k) = x.key.as_deref() {
                walk_expr_purity(k, ctx);
            }
            walk_expr_purity(&x.value, ctx);
            walk_comprehension_clauses(&x.generators, ctx);
        }
        Expr::Dict(x) => {
            for item in &x.items {
                if let Some(k) = &item.key {
                    walk_expr_purity(k, ctx);
                }
                walk_expr_purity(&item.value, ctx);
            }
        }
        Expr::Lambda(x) => {
            if let Some(params) = x.parameters.as_deref() {
                ctx.locals.extend(parameter_names(params));
            }
            walk_expr_purity(&x.body, ctx)
        }
        Expr::FString(x) => {
            for part in x.value.iter() {
                if let ruff_python_ast::FStringPart::FString(f) = part {
                    for elem in f.elements.iter() {
                        if let ruff_python_ast::InterpolatedStringElement::Interpolation(i) = elem {
                            walk_expr_purity(&i.expression, ctx);
                            if let Some(spec) = &i.format_spec {
                                for se in &spec.elements {
                                    if let
                                        ruff_python_ast::InterpolatedStringElement::Interpolation(n)
                                        = se
                                    {
                                        walk_expr_purity(&n.expression, ctx);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Expr::Starred(x) => walk_expr_purity(&x.value, ctx),
        // `Await` / `Yield` / `YieldFrom` are rejected outright by the first
        // arm of this match — they are never walked into.
        Expr::Named(x) => {
            if let Expr::Name(n) = x.target.as_ref() {
                ctx.locals.insert(n.id.as_str().to_owned());
            }
            walk_expr_purity(&x.value, ctx);
        }
        Expr::Slice(x) => {
            for part in [&x.lower, &x.upper, &x.step].into_iter().flatten() {
                walk_expr_purity(part, ctx);
            }
        }
        // A bare name read: fine for parameters, locals, immutable module
        // constants, classes and pure helpers; a proven violation for `mut`
        // module state; unprovable for anything that could change under the
        // function's feet.
        Expr::Name(n) => {
            if n.ctx.is_load() {
                check_name_read(n.id.as_str(), ctx);
            }
        }
        // Leaves: literals carry nothing to inspect.
        Expr::NumberLiteral(_)
        | Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NoneLiteral(_)
        | Expr::EllipsisLiteral(_)
        | Expr::IpyEscapeCommand(_) => {}
        // `t"..."` template strings are not part of the accepted surface;
        // treat like an f-string so a future lowering can't slip past.
        Expr::TString(_) => {}
    }
}

/// Comprehension variables are bound inside the expression; register them
/// as locals so their reads are not mistaken for module-state reads.
fn bind_comprehension_targets(generators: &[ruff_python_ast::Comprehension], ctx: &mut PurityCtx) {
    for g in generators {
        for name in bound_names_in_target(&g.target) {
            ctx.locals.insert(name);
        }
    }
}

/// Walk the iterables and conditions of a comprehension's `for … in … if …`
/// clauses. The element expression is walked by the caller.
fn walk_comprehension_clauses(generators: &[ruff_python_ast::Comprehension], ctx: &mut PurityCtx) {
    for g in generators {
        walk_expr_purity(&g.iter, ctx);
        for cond in &g.ifs {
            walk_expr_purity(cond, ctx);
        }
    }
}

/// A bare-name read inside a pure body.
fn check_name_read(name: &str, ctx: &mut PurityCtx) {
    if ctx.is_param(name) || ctx.is_local(name) {
        return;
    }
    if let Some(state) = ctx.module.bindings.get(name) {
        match state {
            ModuleState::Mutable => ctx.fail(format!(
                "reads mutable module state `{name}` — a `mut` (or rebound) module binding \
                 makes the result depend on when the call happens"
            )),
            ModuleState::Opaque => ctx.unproven(format!(
                "reads module-level `{name}`, whose value could be mutated in place elsewhere"
            )),
            ModuleState::Immutable => {}
        }
        return;
    }
    if ctx.module.class_names.iter().any(|c| c == name) {
        return;
    }
    match ctx.module.user_functions.get(name) {
        Some(true) => return,
        Some(false) => {
            ctx.unproven(format!(
                "passes helper `{name}` around as a value, and `{name}` is not declared pure"
            ));
            return;
        }
        None => {}
    }
    if ctx.module.imports.contains_key(name) {
        ctx.unproven(format!("uses imported `{name}` as a value"));
        return;
    }
    if is_pure_builtin(name) || matches!(name, "True" | "False" | "None" | "Ellipsis") {
        return;
    }
    ctx.unproven(format!(
        "uses `{name}` as a value, which is not provably pure"
    ));
}

/// Check whether an assignment / aug-assign / delete target mutates state
/// the pure function isn't allowed to touch. Bare-name targets are local
/// variables (Python scoping makes them so once `global` is forbidden) and
/// are fine; attribute and subscript targets whose root is a module-level
/// binding or a parameter are not.
fn check_mutation_target(target: &Expr, ctx: &mut PurityCtx) {
    match target {
        Expr::Name(_) => {
            // Local. `Stmt::Global` is already a hard error, so any bare name
            // here is guaranteed to be a function-local binding by Python's
            // own scoping rules.
        }
        Expr::Attribute(_) | Expr::Subscript(_) => {
            if let Some(root) = mutation_root_name(target) {
                if ctx.module.module_names.iter().any(|m| m == &root) && !ctx.is_local(&root) {
                    ctx.fail(format!(
                        "pure functions must not mutate module-level state \
                         (`{}.…` or `{}[…]` would write to a binding declared at module scope)",
                        root, root
                    ));
                } else if ctx.is_param(&root) {
                    ctx.fail(format!(
                        "pure functions must not mutate their arguments \
                         (`{}.…` or `{}[…]` writes to caller-visible state)",
                        root, root
                    ));
                } else if !ctx.fresh.contains(&root) {
                    // A local whose object may alias something the caller
                    // holds (`q = p; q.x = 1`): not provable either way. A
                    // fresh local (`q = Point(...)`) is fine to mutate.
                    ctx.unproven(format!(
                        "writes through local `{root}`, whose object could alias caller-visible state"
                    ));
                }
            } else {
                ctx.unproven("writes through a computed receiver");
            }
        }
        Expr::Tuple(t) => {
            for elt in &t.elts {
                check_mutation_target(elt, ctx);
            }
        }
        Expr::List(l) => {
            for elt in &l.elts {
                check_mutation_target(elt, ctx);
            }
        }
        Expr::Starred(s) => check_mutation_target(&s.value, ctx),
        _ => {}
    }
}

/// Follow an attribute / subscript chain back to its base `Name`. Returns
/// `None` if the chain doesn't bottom out at a single identifier (e.g.
/// `f().attr = 1` — the receiver is a call expression, not a module name).
fn mutation_root_name(target: &Expr) -> Option<String> {
    match target {
        Expr::Name(n) => Some(n.id.as_str().to_owned()),
        Expr::Attribute(a) => mutation_root_name(&a.value),
        Expr::Subscript(s) => mutation_root_name(&s.value),
        _ => None,
    }
}

/// What the walker concluded about one call.
enum CallVerdict {
    Pure,
    Unproven(String),
    Impure(String),
}

/// Classify a call inside a pure body: a bare-name call is checked against
/// the pure builtins, the module's own `@pure` helpers and the pure stdlib
/// allow-list (through the import map); a method call is classified by its
/// receiver — a value of provable builtin type may use that type's
/// non-mutating methods, a module path is checked against the allow-list,
/// and everything else is at best unproven.
fn classify_call(c: &ExprCall, ctx: &PurityCtx) -> CallVerdict {
    if let Some(reason) = forbidden_callee(&c.func, &c.arguments, ctx.module) {
        return CallVerdict::Impure(reason);
    }
    match c.func.as_ref() {
        Expr::Name(n) => {
            let callee = n.id.as_str();
            if ctx.is_param(callee) || ctx.is_local(callee) {
                // A callable parameter or local: nothing is known about it.
                // Rejected outright (as before) so `@pure` cannot launder an
                // arbitrary callback.
                return CallVerdict::Impure(format!(
                    "pure functions may only call pure-builtin helpers or other \
                     `@pure` / `@memo` functions; `{callee}` is a local callable of unknown purity"
                ));
            }
            if let Some(path) = ctx.module.imports.get(callee) {
                return module_path_verdict(path);
            }
            if callee == "next" {
                // `next(it)` advances the iterator: a proven side effect on
                // the caller's object when `it` is a parameter, and merely
                // unprovable when the iterator is a local.
                if let Some(Expr::Name(arg)) = c.arguments.args.first() {
                    if ctx.is_param(arg.id.as_str()) {
                        return CallVerdict::Impure(format!(
                            "pure functions must not mutate their arguments — `next({})` advances the caller's iterator",
                            arg.id.as_str()
                        ));
                    }
                }
                return CallVerdict::Unproven(
                    "`next(...)` advances an iterator, which is not provably pure".to_owned(),
                );
            }
            if matches!(callee, "vars" | "id") {
                return CallVerdict::Unproven(format!(
                    "`{callee}(...)` exposes object identity or live state, which is not provably pure"
                ));
            }
            match check_callee_purity(callee, ctx.module) {
                None => CallVerdict::Pure,
                Some(reason) => CallVerdict::Impure(reason),
            }
        }
        Expr::Attribute(a) => classify_method_call(a, ctx),
        // `fns[i](x)`, `(lambda: ...)()`, `f()(x)`: the callee is computed.
        _ => CallVerdict::Unproven("calls through a computed callee".to_owned()),
    }
}

fn classify_method_call(a: &ruff_python_ast::ExprAttribute, ctx: &PurityCtx) -> CallVerdict {
    let method = a.attr.as_str();
    if is_io_method(method) {
        return CallVerdict::Impure(format!(
            "pure functions must not perform I/O, read clocks or entropy, or mutate resources — `.{method}()` does"
        ));
    }
    let root = mutation_root_name(&a.value);
    let Some(root) = root else {
        // Literal, display, comprehension or call receivers: the value's
        // builtin type is known when the expression makes it obvious.
        return match builtin_head_of_value(&a.value) {
            Some(head) => builtin_method_verdict(head, method),
            None => {
                CallVerdict::Unproven(format!("calls `.{method}()` on a value of unknown type"))
            }
        };
    };
    // Receiver rooted at a name: parameter, local, module binding, import,
    // class, or a builtin type object (`str.join`).
    if let Some(base) = dotted_path(&a.value) {
        let head = base.split('.').next().unwrap_or("");
        if ctx.module.imports.contains_key(head) && !ctx.is_param(head) && !ctx.is_local(head) {
            let path = format!("{base}.{method}");
            return module_path_verdict(&ctx.module.resolve_path(&path));
        }
    }
    if is_logging_method(method) && looks_like_logger(&root) {
        return CallVerdict::Impure(format!(
            "pure functions must not log — `{root}.{method}(...)` writes to a handler"
        ));
    }
    let is_param = ctx.is_param(&root);
    let is_local = ctx.is_local(&root);
    let module_state = if is_param || is_local {
        None
    } else {
        ctx.module.bindings.get(&root).copied()
    };
    if module_state == Some(ModuleState::Mutable) {
        return CallVerdict::Impure(format!(
            "reads mutable module state `{root}` — a `mut` (or rebound) module binding makes the result depend on when the call happens"
        ));
    }
    if is_mutator_method(method) {
        if is_param {
            return CallVerdict::Impure(format!(
                "pure functions must not mutate their arguments — `{root}.{method}(...)` does"
            ));
        }
        if module_state.is_some() {
            return CallVerdict::Impure(format!(
                "pure functions must not mutate module-level state — `{root}.{method}(...)` does"
            ));
        }
        // A local container being built up is unobservable from outside —
        // provided the object is fresh (every initialiser is a display,
        // comprehension, builtin constructor or module class constructor)
        // rather than an alias of something the caller holds.
        if ctx.fresh.contains(&root) {
            return CallVerdict::Pure;
        }
        return CallVerdict::Unproven(format!(
            "mutates local `{root}` via `.{method}()`, which is only pure if `{root}` aliases nothing the caller holds"
        ));
    }
    if let Some(head) = ctx.builtin_type_of(&root) {
        let head = head.to_owned();
        return match module_state {
            Some(ModuleState::Opaque) => CallVerdict::Unproven(format!(
                "reads module-level `{root}`, whose value could be mutated in place elsewhere"
            )),
            _ => builtin_method_verdict(&head, method),
        };
    }
    if builtin_type_head(&root).is_some() && !is_param && !is_local && module_state.is_none() {
        // `str.join(sep, xs)` / `int.from_bytes(...)`: unbound builtin method.
        return builtin_method_verdict(builtin_type_head(&root).unwrap_or(""), method);
    }
    CallVerdict::Unproven(format!(
        "calls `.{method}()` on `{root}`, whose type is not provably a builtin"
    ))
}

/// Verdict for a method of a known builtin type: non-mutating methods are
/// pure, mutators depend on the receiver (handled by the caller), anything
/// else is unproven.
fn builtin_method_verdict(head: &str, method: &str) -> CallVerdict {
    if is_pure_builtin_method(head, method) {
        CallVerdict::Pure
    } else if is_mutator_method(method) {
        CallVerdict::Unproven(format!(
            "`.{method}()` mutates its receiver, which is only pure when the receiver is a fresh local"
        ))
    } else {
        CallVerdict::Unproven(format!(
            "`{head}.{method}()` is not in the pure method table"
        ))
    }
}

/// Verdict for a call resolved to a dotted module path (`math.sqrt`,
/// `datetime.datetime`, `numpy.zeros`).
fn module_path_verdict(path: &str) -> CallVerdict {
    if path_is_in_pure_module(path) {
        CallVerdict::Pure
    } else {
        CallVerdict::Unproven(format!(
            "calls `{path}`, which is outside the pure stdlib allow-list"
        ))
    }
}

/// Stdlib modules whose public callables are pure given pure arguments (no
/// I/O, no clocks, no entropy, no mutation of shared state). Mutable
/// containers built here (`collections.Counter`) are fine to *use* inside a
/// pure body; whether they may be *returned* from a cached function is the
/// return-annotation check's business.
fn path_is_in_pure_module(path: &str) -> bool {
    const PURE_MODULES: &[&str] = &[
        "math",
        "cmath",
        "operator",
        "itertools",
        "functools",
        "string",
        "re",
        "json",
        "statistics",
        "fractions",
        "decimal",
        "numbers",
        "typing",
        "typing_extensions",
        "enum",
        "textwrap",
        "unicodedata",
        "base64",
        "binascii",
        "struct",
        "bisect",
        "heapq",
        "copy",
        "dataclasses",
        "abc",
        "collections",
        "datetime",
        "zoneinfo",
        "hashlib",
        "hmac",
        "difflib",
        "ipaddress",
        "pathlib",
        "urllib.parse",
        "html",
        "keyword",
        "codecs",
        "array",
        "types",
    ];
    if let Some(rest) = path.strip_prefix("os.path.") {
        return matches!(
            rest,
            "join"
                | "basename"
                | "dirname"
                | "splitext"
                | "split"
                | "normpath"
                | "normcase"
                | "commonpath"
                | "commonprefix"
                | "isabs"
                | "splitdrive"
                | "splitroot"
        );
    }
    if path.starts_with("typhon_runtime.") {
        return true;
    }
    PURE_MODULES.iter().any(|m| {
        path == *m
            || path
                .strip_prefix(m)
                .is_some_and(|rest| rest.starts_with('.'))
    })
}

/// Method names that perform I/O, read a clock or entropy source, or drive a
/// resource — impure on any receiver.
fn is_io_method(method: &str) -> bool {
    matches!(
        method,
        "now"
            | "today"
            | "utcnow"
            | "read"
            | "write"
            | "read_text"
            | "write_text"
            | "read_bytes"
            | "write_bytes"
            | "readline"
            | "readlines"
            | "writelines"
            | "readinto"
            | "mkdir"
            | "unlink"
            | "rmdir"
            | "touch"
            | "rename"
            | "send"
            | "sendall"
            | "sendto"
            | "recv"
            | "recvfrom"
            | "connect"
            | "listen"
            | "accept"
            | "execute"
            | "executemany"
            | "commit"
            | "rollback"
            | "put_nowait"
            | "get_nowait"
            | "acquire"
            | "release"
            | "sleep"
            | "flush"
            | "seek"
            | "truncate"
            | "shuffle"
            | "randint"
            | "randrange"
            | "randbytes"
            | "choice"
            | "choices"
            | "sample"
            | "uniform"
            | "gauss"
            | "random"
            | "open"
            | "close"
            | "print"
            | "input"
            | "system"
            | "popen"
            | "kill"
            | "terminate"
            | "urandom"
    )
}

/// Logging-style method names; impure when the receiver looks like a logger.
fn is_logging_method(method: &str) -> bool {
    matches!(
        method,
        "warning"
            | "warn"
            | "info"
            | "debug"
            | "error"
            | "critical"
            | "exception"
            | "fatal"
            | "log"
    )
}

fn looks_like_logger(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "log" || lower == "logging" || lower.ends_with("logger") || lower.ends_with("_log")
}

/// Methods that mutate their receiver in place.
fn is_mutator_method(method: &str) -> bool {
    matches!(
        method,
        "append"
            | "extend"
            | "insert"
            | "pop"
            | "remove"
            | "clear"
            | "sort"
            | "reverse"
            | "update"
            | "setdefault"
            | "popitem"
            | "add"
            | "discard"
            | "appendleft"
            | "popleft"
            | "extendleft"
            | "rotate"
            | "__setitem__"
            | "__delitem__"
            | "__iadd__"
            | "difference_update"
            | "intersection_update"
            | "symmetric_difference_update"
    )
}

/// Non-mutating methods of the builtin types the walker can prove a
/// receiver to be.
fn is_pure_builtin_method(head: &str, method: &str) -> bool {
    match head {
        "str" => matches!(
            method,
            "upper"
                | "lower"
                | "strip"
                | "lstrip"
                | "rstrip"
                | "split"
                | "rsplit"
                | "splitlines"
                | "join"
                | "replace"
                | "startswith"
                | "endswith"
                | "find"
                | "rfind"
                | "index"
                | "rindex"
                | "count"
                | "format"
                | "format_map"
                | "encode"
                | "isdigit"
                | "isalpha"
                | "isalnum"
                | "isspace"
                | "isupper"
                | "islower"
                | "istitle"
                | "isnumeric"
                | "isdecimal"
                | "isidentifier"
                | "isprintable"
                | "isascii"
                | "title"
                | "capitalize"
                | "casefold"
                | "swapcase"
                | "center"
                | "ljust"
                | "rjust"
                | "zfill"
                | "partition"
                | "rpartition"
                | "removeprefix"
                | "removesuffix"
                | "expandtabs"
                | "translate"
                | "maketrans"
        ),
        "bytes" => matches!(
            method,
            "decode"
                | "hex"
                | "fromhex"
                | "upper"
                | "lower"
                | "strip"
                | "split"
                | "join"
                | "replace"
                | "startswith"
                | "endswith"
                | "find"
                | "count"
                | "index"
        ),
        "int" | "bool" => matches!(
            method,
            "bit_length"
                | "bit_count"
                | "to_bytes"
                | "from_bytes"
                | "conjugate"
                | "as_integer_ratio"
        ),
        "float" => matches!(
            method,
            "is_integer" | "as_integer_ratio" | "hex" | "fromhex" | "conjugate"
        ),
        "complex" => matches!(method, "conjugate"),
        "tuple" => matches!(method, "count" | "index"),
        "frozenset" => matches!(
            method,
            "union"
                | "intersection"
                | "difference"
                | "symmetric_difference"
                | "issubset"
                | "issuperset"
                | "isdisjoint"
                | "copy"
        ),
        "list" => matches!(method, "count" | "index" | "copy"),
        "dict" => matches!(method, "get" | "items" | "keys" | "values" | "copy"),
        "set" => matches!(
            method,
            "union"
                | "intersection"
                | "difference"
                | "symmetric_difference"
                | "issubset"
                | "issuperset"
                | "isdisjoint"
                | "copy"
        ),
        "Result" => matches!(
            method,
            "is_ok"
                | "is_err"
                | "unwrap"
                | "unwrap_or"
                | "unwrap_err"
                | "expect"
                | "ok"
                | "err"
                | "map"
                | "map_err"
                | "and_then"
                | "or_else"
                | "unwrap_or_else"
        ),
        _ => false,
    }
}

/// Decide whether a call to bare identifier `name` is permitted inside a
/// pure function body. Returns `None` if the call is fine, or a reason
/// string when it isn't.
///
/// Allowed callees:
/// - Pure builtins (`len`, `abs`, `int`, …) and Typhon constructors
///   (`Ok`, `Err`).
/// - Module-level user functions marked `@pure` / `@memo`.
/// - Module-level class names (their constructors run no side effects in
///   the default dataclass emission).
///
/// Anything else — including impure user functions and unknown identifiers
/// — is rejected so a `@pure` annotation can't paper over an impure callee.
fn check_callee_purity(name: &str, module: &ModuleScope) -> Option<String> {
    if is_pure_builtin(name) {
        return None;
    }
    if module.class_names.iter().any(|c| c == name) {
        return None;
    }
    match module.user_functions.get(name) {
        Some(true) => None,
        Some(false) => Some(format!(
            "pure functions must not call impure helper `{name}` — \
             mark `{name}` with `@pure` (or `@memo`) for transitive purity"
        )),
        None => {
            // Unknown identifier: either an imported function (we can't know
            // its purity without per-import metadata) or a name shadowed by a
            // parameter / local binding. Be conservative — reject anything
            // we can't prove pure rather than risk a false negative.
            Some(format!(
                "pure functions may only call pure-builtin helpers or other \
                 `@pure` / `@memo` functions; `{name}` is neither"
            ))
        }
    }
}

/// Conservative allow-list of CPython builtins whose contract is pure (no
/// I/O, no clocks, no entropy, no mutation of arguments). Constructors —
/// including the mutable containers `list` / `dict` / `set` / `bytearray`,
/// which are as pure to *build* as a display is — and basic transformations
/// are included. Lazy iterator builtins (`map`, `filter`, `zip`, ...) are
/// pure to use; whether a mutable or lazy result may escape a *cached*
/// function is the return-annotation check's business. `next` / `vars` /
/// `id` are deliberately absent: they advance or expose state the walker
/// cannot see.
fn is_pure_builtin(name: &str) -> bool {
    matches!(
        name,
        // Transformations / queries on immutable args.
        "abs" | "all" | "any" | "ascii" | "bin" | "bool" | "callable"
        | "chr" | "complex" | "divmod" | "enumerate" | "filter"
        | "float" | "format" | "frozenset" | "hasattr" | "hash" | "hex"
        | "int" | "isinstance" | "issubclass" | "iter" | "len"
        | "map" | "max" | "min" | "oct" | "ord" | "pow"
        | "range" | "repr" | "reversed" | "round" | "sorted" | "str"
        | "sum" | "tuple" | "type" | "zip" | "bytes" | "object"
        | "list" | "dict" | "set" | "bytearray" | "slice"
        // Typhon `Result` constructors emitted by the desugar pass.
        | "Ok" | "Err" | "Result"
    )
}

/// If `func` references a forbidden callable (I/O, entropy, clock), return a
/// concrete reason string. Otherwise return `None`. Import aliases are
/// expanded first, so `from datetime import datetime; datetime.now()` and
/// `import numpy as np; np.random.rand()` are both seen for what they are.
fn forbidden_callee(
    func: &Expr,
    args: &ruff_python_ast::Arguments,
    module: &ModuleScope,
) -> Option<String> {
    let raw = dotted_path(func)?;
    let path = module.resolve_path(&raw);
    // Bare-name builtins.
    if !path.contains('.') {
        match path.as_str() {
            "print" | "open" | "input" | "exec" | "eval" | "compile" | "breakpoint" | "exit"
            | "quit" | "setattr" | "delattr" | "globals" | "locals" | "__import__" => {
                return Some(format!(
                    "pure functions must not call `{path}` — it performs I/O or mutates state"
                ));
            }
            _ => return None,
        }
    }
    // Pure carve-outs under otherwise impure prefixes.
    if path_is_in_pure_module(&path) && !path.starts_with("datetime.") {
        return None;
    }
    let last = path.rsplit('.').next().unwrap_or("");
    if matches!(last, "now" | "today" | "utcnow") {
        return Some(format!(
            "pure functions must not read a clock — `{path}` is non-deterministic"
        ));
    }
    if path.starts_with("datetime.") {
        return None;
    }
    if let Some(rest) = path.strip_prefix("time.") {
        let no_time_arg = args.args.is_empty() && args.keywords.is_empty();
        let clock_read = match rest {
            "gmtime" | "localtime" | "ctime" | "asctime" => no_time_arg,
            "strftime" => args.args.len() + args.keywords.len() < 2,
            "strptime" | "mktime" | "struct_time" => false,
            _ => true,
        };
        if clock_read {
            return Some(format!(
                "pure functions must not read a clock or sleep — `{path}` is non-deterministic"
            ));
        }
        return None;
    }
    if path.starts_with("random.")
        || path.starts_with("secrets.")
        || path.starts_with("uuid.uuid")
        || path == "os.urandom"
        || path.split('.').any(|seg| seg == "random")
    {
        return Some(format!(
            "pure functions must not read entropy — `{path}` is non-deterministic"
        ));
    }
    // Stdlib module attributes.
    const IO_PREFIXES: &[&str] = &[
        "os.",
        "sys.stdout",
        "sys.stderr",
        "sys.stdin",
        "sys.exit",
        "sys.settrace",
        "sys.setrecursionlimit",
        "subprocess.",
        "socket.",
        "requests.",
        "urllib.",
        "httpx.",
        "aiohttp.",
        "logging.",
        "shutil.",
        "tempfile.",
        "glob.",
        "sqlite3.",
        "threading.",
        "multiprocessing.",
        "signal.",
        "atexit.",
        "getpass.",
        "webbrowser.",
        "smtplib.",
        "ftplib.",
        "http.",
        "select.",
        "selectors.",
        "asyncio.",
        "builtins.print",
        "builtins.open",
        "builtins.input",
    ];
    if IO_PREFIXES.iter().any(|p| path.starts_with(p)) {
        return Some(format!(
            "pure functions must not perform I/O — call `{path}` is impure"
        ));
    }
    None
}

fn dotted_path(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name(n) => Some(n.id.as_str().to_owned()),
        Expr::Attribute(a) => {
            let base = dotted_path(&a.value)?;
            Some(format!("{}.{}", base, a.attr.as_str()))
        }
        _ => None,
    }
}

/// Render the `PurityFinding` list into diagnostics. Only explicit `@pure` /
/// `@memo` opt-ins produce hard errors when they fail the purity check —
/// auto-memoise findings stay silent (they only feed cache-decorator
/// injection on the functions that happen to pass). Returns the diagnostics
/// list; callers can still consult the findings vector unmodified to drive
/// memoise targets.
pub fn purity_diagnostics(findings: &[PurityFinding], path: &str, source: &str) -> Diagnostics {
    let mut diags = Diagnostics::new();
    for f in findings {
        if !f.declared_pure {
            continue;
        }
        if let Some(reason) = &f.violation {
            diags.push_error(TycError::impure_pure_fn(
                f.name.clone(),
                reason.clone(),
                path,
                source,
                f.span.0,
                f.span.1.saturating_sub(f.span.0).max(1),
            ));
        }
    }
    diags
}

// ── Lint passes ───────────────────────────────────────────────────────────────
//
// Lightweight syntactic warnings that supplement the main type-checker.  All
// of these emit `severity(Warning)` diagnostics; they never block a build on
// their own.  Each pass walks the parsed module once and collects findings
// into a [`Diagnostics`] bundle.

/// Walk every top-level / nested binding statement in `module` and warn when
/// a `let` / module-level assignment whose name matches the secret-suffix
/// heuristic is initialised from a raw string literal.
///
/// Pattern (case-insensitive on the name): the binding identifier ends in
/// `_TOKEN`, `_SECRET`, `_PASSWORD`, `_PWD`, `_KEY`, or `_API_KEY` (or the
/// suffix _is_ the whole name — `KEY`, `TOKEN`, …). The RHS must be a bare
/// string literal; any non-literal RHS (function call, attribute access,
/// `os.getenv("…")`) is fine because it's likely runtime-driven.
///
/// Only fires for plain assignments — `comptime let X = env("…")` already
/// has its own `contains_secret_literal` path inside `tyc build`, so this
/// pass deliberately skips comptime bindings (their RHS is substituted out
/// at build time anyway).
pub fn analyse_secret_literal_bindings(
    module: &ModModule,
    path: &str,
    source: &str,
    allow_secret_comptime: bool,
) -> Diagnostics {
    let mut diags = Diagnostics::new();
    if !allow_secret_comptime {
        walk_secret_literal_stmts(&module.body, path, source, &mut diags);
    }
    diags
}

fn walk_secret_literal_stmts(body: &[Stmt], path: &str, source: &str, diags: &mut Diagnostics) {
    for stmt in body {
        match stmt {
            Stmt::Assign(a) => {
                // `X = "literal"` — every name target on the LHS counts as a
                // candidate, including tuple-unpacks like `(API_KEY, b) = …`.
                if !is_string_literal(&a.value) {
                    continue;
                }
                for target in &a.targets {
                    record_secret_targets(target, source, path, diags);
                }
            }
            Stmt::AnnAssign(a) => {
                let Some(rhs) = a.value.as_deref() else {
                    continue;
                };
                if !is_string_literal(rhs) {
                    continue;
                }
                if let Expr::Name(n) = a.target.as_ref() {
                    if is_secret_name(n.id.as_str()) {
                        let span_start = n.range.start().to_usize();
                        let length = n.id.as_str().len();
                        diags.push_warning(TycError::secret_literal_inline(
                            n.id.as_str().to_owned(),
                            path,
                            source.to_owned(),
                            span_start,
                            length,
                        ));
                    }
                }
            }
            Stmt::FunctionDef(f) => walk_secret_literal_stmts(&f.body, path, source, diags),
            Stmt::ClassDef(c) => walk_secret_literal_stmts(&c.body, path, source, diags),
            Stmt::If(i) => {
                walk_secret_literal_stmts(&i.body, path, source, diags);
                for clause in &i.elif_else_clauses {
                    walk_secret_literal_stmts(&clause.body, path, source, diags);
                }
            }
            Stmt::While(w) => {
                walk_secret_literal_stmts(&w.body, path, source, diags);
                walk_secret_literal_stmts(&w.orelse, path, source, diags);
            }
            Stmt::For(f) => {
                walk_secret_literal_stmts(&f.body, path, source, diags);
                walk_secret_literal_stmts(&f.orelse, path, source, diags);
            }
            Stmt::With(w) => walk_secret_literal_stmts(&w.body, path, source, diags),
            Stmt::Try(t) => {
                walk_secret_literal_stmts(&t.body, path, source, diags);
                walk_secret_literal_stmts(&t.orelse, path, source, diags);
                walk_secret_literal_stmts(&t.finalbody, path, source, diags);
                for h in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    walk_secret_literal_stmts(&h.body, path, source, diags);
                }
            }
            _ => {}
        }
    }
}

fn record_secret_targets(target: &Expr, source: &str, path: &str, diags: &mut Diagnostics) {
    match target {
        Expr::Name(n) if is_secret_name(n.id.as_str()) => {
            let span_start = n.range.start().to_usize();
            let length = n.id.as_str().len();
            diags.push_warning(TycError::secret_literal_inline(
                n.id.as_str().to_owned(),
                path,
                source.to_owned(),
                span_start,
                length,
            ));
        }
        Expr::Tuple(t) => {
            for elt in &t.elts {
                record_secret_targets(elt, source, path, diags);
            }
        }
        Expr::List(l) => {
            for elt in &l.elts {
                record_secret_targets(elt, source, path, diags);
            }
        }
        _ => {}
    }
}

/// True when `expr` is a bare string literal (`"foo"`, `'bar'`,
/// `"""…"""`). Concatenations (`"a" + "b"`) and f-strings are intentionally
/// NOT treated as literals — those forms suggest the user is composing the
/// value programmatically, even if the result is statically constant.
fn is_string_literal(expr: &Expr) -> bool {
    matches!(expr, Expr::StringLiteral(_))
}

/// Secret-shaped name keywords, longest / most-specific first.
///
/// The single source of truth shared by the `tyc::contains_secret_literal`
/// lint in this crate ([`is_secret_name`]) and the `tyc build` secret-suffix
/// scan (`secret_suffix` in the CLI crate), so the two heuristics cannot
/// drift apart — exactly the class of bug fixed in v1.0.0-alpha.4, where an
/// ordering discrepancy between the two copies made `KEY_APIKEY` report the
/// less-specific suffix. Invariant: any keyword that contains another
/// keyword as a substring (`APITOKEN` ⊃ `TOKEN`, `API_KEY` ⊃ `KEY`) must
/// come first, so first-match reporting picks the most specific word.
/// That ordering is asserted by `secret_keyword_table_is_longest_first`
/// (this crate's tests) — reorder freely, the test catches violations.
pub const SECRET_NAME_KEYWORDS: &[&str] = &[
    // Longest-first: `APIKEY` must be tried before the bare `KEY` so a name
    // like `KEY_APIKEY` reports the more specific suffix. `PASSPHRASE` must
    // precede `PASS` for the same reason — and it needs its own entry at all
    // because the word-boundary rule that (correctly) stops `PASSPORT` from
    // matching `PASS` also stopped `PASSPHRASE`, so the single most obvious
    // secret-shaped name after `PASSWORD` went unflagged. `PRIVKEY` is here
    // for exactly that reason too: the `V`→`K` junction is not a word
    // boundary, so `PRIVKEY` / `SSH_PRIVKEY` / `PRIVKEY_PEM` never matched
    // the bare `KEY`. It precedes `KEY` so the specific word is reported.
    "PASSPHRASE",
    "AUTHORIZATION",
    "CREDENTIALS",
    "CREDENTIAL",
    "WEBHOOK",
    "SIGNING",
    "COOKIE",
    "DB_PASSWORD",
    "DBPASSWORD",
    "DB_PASS",
    "DBPASS",
    "DB_PWD",
    "DBPWD",
    "API_PASSWORD",
    "APIPASSWORD",
    "DB_SECRET",
    "DBSECRET",
    "API_SECRET",
    "APISECRET",
    "APP_SECRET",
    "APPSECRET",
    "CLIENT_SECRET",
    "CLIENTSECRET",
    "JWT_SECRET",
    "JWTSECRET",
    "SECRET_KEY",
    "SECRETKEY",
    "ACCESS_TOKEN",
    "ACCESSTOKEN",
    "AUTH_TOKEN",
    "AUTHTOKEN",
    "BEARER_TOKEN",
    "BEARERTOKEN",
    "CSRF_TOKEN",
    "CSRFTOKEN",
    "JWT_TOKEN",
    "JWTTOKEN",
    "API_TOKEN",
    "APITOKEN",
    "PASSWORD",
    "SECRET",
    "TOKEN",
    "PRIVATE_KEY",
    "PRIVATEKEY",
    "PUBLIC_KEY",
    "PUBLICKEY",
    "SSH_KEY",
    "SSHKEY",
    "API_KEY",
    "APIKEY",
    "PRIVKEY",
    "KEY",
    "PWD",
    "PASS",
    "DSN",
];

/// Return the first (i.e. most specific, given the table's longest-first
/// ordering) secret-shaped keyword that occurs in `name` as a bounded
/// substring, or `None`. Match is case-insensitive; the keyword may form
/// the whole name (e.g. `TOKEN`), follow an underscore (e.g. `MY_TOKEN`),
/// sit at a digit or camelCase/PascalCase boundary (e.g. `myTokenValue`,
/// `foo123TOKEN`), or start/end the name.
///
/// This is the ONE implementation of the word-boundary heuristic, shared by
/// the `tyc::contains_secret_literal` lint here ([`is_secret_name`]) and the
/// `tyc build` secret-suffix scan (`secret_suffix` in the CLI crate). The two
/// consumers previously carried hand-synchronised copies of this logic —
/// every new boundary rule (alpha.8's digit / TitleCase junctions, alpha.9's
/// uppercase→lowercase junction) had to be applied to both by hand, the same
/// drift class that produced the alpha.4 `KEY_APIKEY` ordering bug.
pub fn secret_keyword_match(name: &str) -> Option<&'static str> {
    let upper = name.to_ascii_uppercase();
    for word in SECRET_NAME_KEYWORDS {
        let mut start_idx = 0;
        while let Some(idx) = upper[start_idx..].find(word) {
            let actual_idx = start_idx + idx;
            // Ensure the word is bounded by string start/end or underscores,
            // or preceded/followed by a casing change (for camelCase like `myTokenValue`).
            // so `MONKEY` doesn't match `KEY` and `PASSPORT` doesn't match `PASS`.
            let start_ok = actual_idx == 0
                || upper.as_bytes()[actual_idx - 1] == b'_'
                || name.as_bytes()[actual_idx - 1].is_ascii_digit()
                || (name.as_bytes()[actual_idx].is_ascii_uppercase()
                    && name.as_bytes()[actual_idx - 1].is_ascii_lowercase());
            let actual_end = actual_idx + word.len();
            let end_ok = actual_end == upper.len()
                || upper.as_bytes()[actual_end] == b'_'
                || name.as_bytes()[actual_end].is_ascii_digit()
                || (name.as_bytes()[actual_end].is_ascii_uppercase()
                    && !name.as_bytes()[actual_end - 1].is_ascii_uppercase())
                || (name.as_bytes()[actual_end].is_ascii_uppercase()
                    && actual_end + 1 < name.len()
                    && name.as_bytes()[actual_end + 1].is_ascii_lowercase())
                || (name.as_bytes()[actual_end].is_ascii_lowercase()
                    && name.as_bytes()[actual_end - 1].is_ascii_uppercase());
            if start_ok && end_ok {
                return Some(word);
            }
            start_idx = actual_idx + 1;
        }
    }
    None
}

/// True when `name` contains one of the recognised secret-shaped keywords
/// as a bounded substring — see [`secret_keyword_match`] for the boundary
/// rules. The expected callers feed module-level binding names; passing a
/// function or method name through is harmless because the caller already
/// gated on `Stmt::Assign` / `Stmt::AnnAssign`.
fn is_secret_name(name: &str) -> bool {
    secret_keyword_match(name).is_some()
}

/// Walk every `let` / `mut` / `AnnAssign` binding statement in `module`
/// and warn when the RHS is an empty collection literal (`[]`, `{}`,
/// `set()`) AND the binding carries no explicit type annotation.
///
/// Without an annotation the type checker falls back to `list[Unknown]` /
/// `dict[Unknown, Unknown]` / `set[Unknown]`, which behaves like `Any` and
/// silences later element-type mismatches. Annotated bindings (`let
/// xs: list[int] = []`) are fine because the annotation pins the element
/// type — they are NOT warned about. This pass deliberately does not
/// touch inference; it just surfaces the silent-`Any` case so the user
/// can opt into stricter typing by annotating.
pub fn analyse_empty_collection_bindings(
    module: &ModModule,
    path: &str,
    source: &str,
) -> Diagnostics {
    let mut diags = Diagnostics::new();
    walk_empty_collection_stmts(&module.body, path, source, &mut diags);
    diags
}

fn walk_empty_collection_stmts(body: &[Stmt], path: &str, source: &str, diags: &mut Diagnostics) {
    for stmt in body {
        match stmt {
            Stmt::Assign(a) => {
                // Unannotated `X = []` / `X = {}` / `X = set()`.
                let Some(literal) = empty_collection_kind(&a.value) else {
                    continue;
                };
                for target in &a.targets {
                    if let Expr::Name(n) = target {
                        let span_start = n.range.start().to_usize();
                        let length = n.id.as_str().len();
                        diags.push_warning(TycError::empty_collection_no_annotation(
                            n.id.as_str().to_owned(),
                            literal,
                            path,
                            source.to_owned(),
                            span_start,
                            length,
                        ));
                    }
                }
            }
            // `AnnAssign` carries an explicit annotation — never warns.
            Stmt::FunctionDef(f) => walk_empty_collection_stmts(&f.body, path, source, diags),
            Stmt::ClassDef(c) => walk_empty_collection_stmts(&c.body, path, source, diags),
            Stmt::If(i) => {
                walk_empty_collection_stmts(&i.body, path, source, diags);
                for clause in &i.elif_else_clauses {
                    walk_empty_collection_stmts(&clause.body, path, source, diags);
                }
            }
            Stmt::While(w) => {
                walk_empty_collection_stmts(&w.body, path, source, diags);
                walk_empty_collection_stmts(&w.orelse, path, source, diags);
            }
            Stmt::For(f) => {
                walk_empty_collection_stmts(&f.body, path, source, diags);
                walk_empty_collection_stmts(&f.orelse, path, source, diags);
            }
            Stmt::With(w) => walk_empty_collection_stmts(&w.body, path, source, diags),
            Stmt::Try(t) => {
                walk_empty_collection_stmts(&t.body, path, source, diags);
                walk_empty_collection_stmts(&t.orelse, path, source, diags);
                walk_empty_collection_stmts(&t.finalbody, path, source, diags);
                for h in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    walk_empty_collection_stmts(&h.body, path, source, diags);
                }
            }
            _ => {}
        }
    }
}

/// Classify `expr` as one of the recognised empty-collection literal
/// forms. Returns the human-readable label used in the diagnostic
/// message, or `None` when `expr` isn't an empty literal.
///
/// Recognised forms:
///   - `[]` — empty list literal.
///   - `{}` — empty dict literal (note: there is no empty-set literal in
///     Python; `{}` is always a dict).
///   - `set()` — bare call to `set` with no arguments.
fn empty_collection_kind(expr: &Expr) -> Option<&'static str> {
    match expr {
        Expr::List(l) if l.elts.is_empty() => Some("list literal `[]`"),
        Expr::Dict(d) if d.items.is_empty() => Some("dict literal `{}`"),
        Expr::Call(c) if c.arguments.args.is_empty() && c.arguments.keywords.is_empty() => {
            if let Expr::Name(n) = c.func.as_ref() {
                if n.id.as_str() == "set" {
                    return Some("set literal `set()`");
                }
            }
            None
        }
        _ => None,
    }
}

/// Mutable-default-parameter lint (`tyc::mutable_default_param`): a
/// `def f(xs: list[int] = [])` evaluates the literal once at definition
/// time, so every defaulted call mutates the same shared object. Walks
/// every function (including nested ones and methods inside class /
/// impl bodies) and flags list / dict / set / mutable-constructor
/// defaults on parameters.
pub fn analyse_mutable_default_params(module: &ModModule, path: &str, source: &str) -> Diagnostics {
    let mut diags = Diagnostics::new();
    walk_mutable_default_stmts(&module.body, path, source, &mut diags);
    diags
}

/// What makes a parameter default *mutable*: literal lists / dicts /
/// sets / comprehensions, and calls to the mutable builtin constructors.
/// Non-empty literals are just as shared as empty ones, so unlike
/// [`empty_collection_kind`] this matches any arity.
fn mutable_default_kind(expr: &Expr) -> Option<&'static str> {
    match expr {
        Expr::List(_) => Some("list literal"),
        Expr::Dict(_) => Some("dict literal"),
        Expr::Set(_) => Some("set literal"),
        Expr::ListComp(_) => Some("list comprehension"),
        Expr::DictComp(_) => Some("dict comprehension"),
        Expr::SetComp(_) => Some("set comprehension"),
        Expr::Call(c) => {
            if let Expr::Name(n) = c.func.as_ref() {
                match n.id.as_str() {
                    "list" => Some("`list()` constructor call"),
                    "dict" => Some("`dict()` constructor call"),
                    "set" => Some("`set()` constructor call"),
                    "bytearray" => Some("`bytearray()` constructor call"),
                    _ => None,
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

fn walk_mutable_default_stmts(body: &[Stmt], path: &str, source: &str, diags: &mut Diagnostics) {
    use ruff_text_size::Ranged;
    for stmt in body {
        match stmt {
            Stmt::FunctionDef(f) => {
                let all_params = f
                    .parameters
                    .posonlyargs
                    .iter()
                    .chain(f.parameters.args.iter())
                    .chain(f.parameters.kwonlyargs.iter());
                for p in all_params {
                    let Some(default) = p.default.as_deref() else {
                        continue;
                    };
                    if let Some(kind) = mutable_default_kind(default) {
                        let span_start = default.range().start().to_usize();
                        let length = default.range().end().to_usize() - span_start;
                        diags.push_warning(TycError::mutable_default_param(
                            p.parameter.name.as_str(),
                            kind,
                            path,
                            source.to_owned(),
                            span_start,
                            length,
                        ));
                    }
                }
                walk_mutable_default_stmts(&f.body, path, source, diags);
            }
            Stmt::ClassDef(c) => walk_mutable_default_stmts(&c.body, path, source, diags),
            Stmt::If(s) => {
                walk_mutable_default_stmts(&s.body, path, source, diags);
                for clause in &s.elif_else_clauses {
                    walk_mutable_default_stmts(&clause.body, path, source, diags);
                }
            }
            Stmt::While(s) => walk_mutable_default_stmts(&s.body, path, source, diags),
            Stmt::For(s) => walk_mutable_default_stmts(&s.body, path, source, diags),
            Stmt::With(s) => walk_mutable_default_stmts(&s.body, path, source, diags),
            Stmt::Try(t) => {
                walk_mutable_default_stmts(&t.body, path, source, diags);
                for h in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    walk_mutable_default_stmts(&h.body, path, source, diags);
                }
                walk_mutable_default_stmts(&t.orelse, path, source, diags);
                walk_mutable_default_stmts(&t.finalbody, path, source, diags);
            }
            _ => {}
        }
    }
}

/// `is` / `is not` against a literal operand (`tyc::is_literal_comparison`):
/// identity comparison with a fresh literal is interpreter-dependent —
/// CPython itself SyntaxWarns on it. Walks every comparison expression.
pub fn analyse_is_literal_comparisons(module: &ModModule, path: &str, source: &str) -> Diagnostics {
    use ruff_python_ast::visitor::source_order::{walk_expr, SourceOrderVisitor};
    use ruff_text_size::Ranged;
    struct V<'a> {
        path: &'a str,
        source: &'a str,
        diags: &'a mut Diagnostics,
    }
    fn is_value_literal(e: &Expr) -> bool {
        matches!(
            e,
            Expr::StringLiteral(_)
                | Expr::NumberLiteral(_)
                | Expr::BytesLiteral(_)
                | Expr::FString(_)
        )
    }
    impl<'a, 'b> SourceOrderVisitor<'a> for V<'b> {
        fn visit_expr(&mut self, e: &'a Expr) {
            if let Expr::Compare(cmp) = e {
                let mut left: &Expr = &cmp.left;
                for (op, right) in cmp.ops.iter().zip(cmp.comparators.iter()) {
                    if matches!(
                        op,
                        ruff_python_ast::CmpOp::Is | ruff_python_ast::CmpOp::IsNot
                    ) {
                        let lit = if is_value_literal(left) {
                            Some(left)
                        } else if is_value_literal(right) {
                            Some(right)
                        } else {
                            None
                        };
                        if let Some(lit) = lit {
                            let start = lit.range().start().to_usize();
                            let len = lit.range().end().to_usize() - start;
                            self.diags.push_warning(TycError::is_literal_comparison(
                                self.path,
                                self.source.to_owned(),
                                start,
                                len,
                            ));
                        }
                    }
                    left = right;
                }
            }
            walk_expr(self, e);
        }
    }
    let mut diags = Diagnostics::new();
    {
        let mut v = V {
            path,
            source,
            diags: &mut diags,
        };
        for stmt in &module.body {
            v.visit_stmt(stmt);
        }
    }
    diags
}

/// Late-binding closure lint (`tyc::loop_closure_capture`): a lambda or
/// nested `def` created inside a `for` loop that references the loop
/// variable captures the *binding*, not the iteration's value — every
/// deferred call observes the final value. Immediately-invoked lambdas
/// (`(lambda: i)()`) and parameters that shadow the loop name (the
/// `lambda i=i:` idiom) are exempt.
pub fn analyse_loop_closure_captures(module: &ModModule, path: &str, source: &str) -> Diagnostics {
    let mut diags = Diagnostics::new();

    fn target_names(e: &Expr, into: &mut Vec<String>) {
        match e {
            Expr::Name(n) => into.push(n.id.as_str().to_owned()),
            Expr::Tuple(t) => {
                for el in &t.elts {
                    target_names(el, into);
                }
            }
            Expr::List(l) => {
                for el in &l.elts {
                    target_names(el, into);
                }
            }
            Expr::Starred(s) => target_names(&s.value, into),
            _ => {}
        }
    }

    /// Collect references to `names` inside a closure body, skipping any
    /// name shadowed by the closure's own parameters.
    #[allow(clippy::type_complexity)]
    fn flag_captures_in_closure(
        params: Option<&Parameters>,
        body_exprs: &mut dyn FnMut(&mut dyn FnMut(&Expr)),
        names: &[String],
        path: &str,
        source: &str,
        diags: &mut Diagnostics,
    ) {
        use ruff_text_size::Ranged;
        let mut shadowed: Vec<&str> = params
            .map(|p| {
                p.posonlyargs
                    .iter()
                    .chain(p.args.iter())
                    .chain(p.kwonlyargs.iter())
                    .map(|a| a.parameter.name.as_str())
                    .collect()
            })
            .unwrap_or_default();
        // `*args` / `**kwargs` shadow too.
        if let Some(p) = params {
            if let Some(va) = &p.vararg {
                shadowed.push(va.name.as_str());
            }
            if let Some(ka) = &p.kwarg {
                shadowed.push(ka.name.as_str());
            }
        }
        let mut visit = |e: &Expr| {
            walk_names(e, &mut |n: &ruff_python_ast::ExprName| {
                let id = n.id.as_str();
                if names.iter().any(|t| t == id) && !shadowed.contains(&id) {
                    let start = n.range().start().to_usize();
                    let len = id.len();
                    diags.push_warning(TycError::loop_closure_capture(
                        id,
                        path,
                        source.to_owned(),
                        start,
                        len,
                    ));
                }
            });
        };
        body_exprs(&mut visit);
    }

    /// Walk an expression tree calling `f` on every Name (load) node,
    /// without descending into nested lambdas (their own pass handles
    /// them — and their params may shadow).
    fn walk_names(e: &Expr, f: &mut dyn FnMut(&ruff_python_ast::ExprName)) {
        use ruff_python_ast::visitor::source_order::{walk_expr, SourceOrderVisitor};
        struct V<'a> {
            f: &'a mut dyn FnMut(&ruff_python_ast::ExprName),
        }
        impl<'a, 'b> SourceOrderVisitor<'a> for V<'b> {
            fn visit_expr(&mut self, e: &'a Expr) {
                if let Expr::Name(n) = e {
                    (self.f)(n);
                    return;
                }
                if matches!(e, Expr::Lambda(_)) {
                    return;
                }
                walk_expr(self, e);
            }
        }
        let mut v = V { f };
        v.visit_expr(e);
    }

    /// Find closures in an expression tree (skipping immediately-invoked
    /// ones) and flag loop-name captures inside them.
    fn scan_expr_for_closures(
        e: &Expr,
        names: &[String],
        path: &str,
        source: &str,
        diags: &mut Diagnostics,
    ) {
        match e {
            Expr::Lambda(lam) => {
                let body = lam.body.clone();
                flag_captures_in_closure(
                    lam.parameters.as_deref(),
                    &mut |visit| visit(&body),
                    names,
                    path,
                    source,
                    diags,
                );
                // Still scan the lambda body for *nested* closures.
                scan_expr_for_closures(&lam.body, names, path, source, diags);
            }
            Expr::Call(c) => {
                // Immediately-invoked lambda: exempt the lambda itself but
                // scan its arguments.
                if matches!(c.func.as_ref(), Expr::Lambda(_)) {
                    for a in c.arguments.args.iter() {
                        scan_expr_for_closures(a, names, path, source, diags);
                    }
                    for k in c.arguments.keywords.iter() {
                        scan_expr_for_closures(&k.value, names, path, source, diags);
                    }
                    return;
                }
                scan_expr_for_closures(&c.func, names, path, source, diags);
                for a in c.arguments.args.iter() {
                    scan_expr_for_closures(a, names, path, source, diags);
                }
                for k in c.arguments.keywords.iter() {
                    scan_expr_for_closures(&k.value, names, path, source, diags);
                }
            }
            other => {
                use ruff_python_ast::visitor::source_order::{walk_expr, SourceOrderVisitor};
                struct V<'a> {
                    names: &'a [String],
                    path: &'a str,
                    source: &'a str,
                    diags: &'a mut Diagnostics,
                }
                impl<'a, 'b> SourceOrderVisitor<'a> for V<'b> {
                    fn visit_expr(&mut self, e: &'a Expr) {
                        if matches!(e, Expr::Lambda(_) | Expr::Call(_)) {
                            scan_expr_for_closures(
                                e,
                                self.names,
                                self.path,
                                self.source,
                                self.diags,
                            );
                            return;
                        }
                        walk_expr(self, e);
                    }
                }
                let mut v = V {
                    names,
                    path,
                    source,
                    diags,
                };
                v.visit_expr(other);
            }
        }
    }

    fn scan_stmts_for_closures(
        stmts: &[Stmt],
        names: &[String],
        path: &str,
        source: &str,
        diags: &mut Diagnostics,
    ) {
        for stmt in stmts {
            match stmt {
                Stmt::FunctionDef(f) => {
                    // A nested def in the loop body is a closure too.
                    let names_owned = names.to_vec();
                    let body = &f.body;
                    flag_captures_in_closure(
                        Some(&f.parameters),
                        &mut |visit| {
                            for s in body.iter() {
                                visit_stmt_exprs(s, visit);
                            }
                        },
                        &names_owned,
                        path,
                        source,
                        diags,
                    );
                }
                Stmt::Expr(e) => scan_expr_for_closures(&e.value, names, path, source, diags),
                Stmt::Assign(a) => scan_expr_for_closures(&a.value, names, path, source, diags),
                Stmt::AnnAssign(a) => {
                    if let Some(v) = &a.value {
                        scan_expr_for_closures(v, names, path, source, diags);
                    }
                }
                Stmt::Return(r) => {
                    if let Some(v) = &r.value {
                        scan_expr_for_closures(v, names, path, source, diags);
                    }
                }
                Stmt::If(s) => {
                    // Header use-positions can themselves hold a closure
                    // (`if (lambda: i)():`), so scan the test expressions too.
                    scan_expr_for_closures(&s.test, names, path, source, diags);
                    scan_stmts_for_closures(&s.body, names, path, source, diags);
                    for cl in &s.elif_else_clauses {
                        if let Some(test) = &cl.test {
                            scan_expr_for_closures(test, names, path, source, diags);
                        }
                        scan_stmts_for_closures(&cl.body, names, path, source, diags);
                    }
                }
                Stmt::While(s) => {
                    scan_expr_for_closures(&s.test, names, path, source, diags);
                    scan_stmts_for_closures(&s.body, names, path, source, diags);
                    scan_stmts_for_closures(&s.orelse, names, path, source, diags);
                }
                Stmt::For(s) => {
                    scan_expr_for_closures(&s.iter, names, path, source, diags);
                    scan_stmts_for_closures(&s.body, names, path, source, diags);
                    scan_stmts_for_closures(&s.orelse, names, path, source, diags);
                }
                Stmt::With(s) => {
                    for item in &s.items {
                        scan_expr_for_closures(&item.context_expr, names, path, source, diags);
                    }
                    scan_stmts_for_closures(&s.body, names, path, source, diags);
                }
                Stmt::Try(t) => {
                    scan_stmts_for_closures(&t.body, names, path, source, diags);
                    for h in &t.handlers {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                        if let Some(ty) = &h.type_ {
                            scan_expr_for_closures(ty, names, path, source, diags);
                        }
                        scan_stmts_for_closures(&h.body, names, path, source, diags);
                    }
                    scan_stmts_for_closures(&t.orelse, names, path, source, diags);
                    scan_stmts_for_closures(&t.finalbody, names, path, source, diags);
                }
                Stmt::Match(m) => {
                    scan_expr_for_closures(&m.subject, names, path, source, diags);
                    for case in &m.cases {
                        if let Some(guard) = &case.guard {
                            scan_expr_for_closures(guard, names, path, source, diags);
                        }
                        scan_stmts_for_closures(&case.body, names, path, source, diags);
                    }
                }
                _ => {}
            }
        }
    }

    /// Visit every expression in a statement (shallow walk that recurses
    /// through nested control flow but not into nested defs/lambdas —
    /// `flag_captures_in_closure` handles shadowing per closure).
    fn visit_stmt_exprs(stmt: &Stmt, f: &mut dyn FnMut(&Expr)) {
        match stmt {
            Stmt::Expr(e) => f(&e.value),
            Stmt::Assign(a) => f(&a.value),
            Stmt::AnnAssign(a) => {
                if let Some(v) = &a.value {
                    f(v);
                }
            }
            Stmt::Return(r) => {
                if let Some(v) = &r.value {
                    f(v);
                }
            }
            Stmt::If(s) => {
                f(&s.test);
                for st in &s.body {
                    visit_stmt_exprs(st, f);
                }
                for cl in &s.elif_else_clauses {
                    if let Some(t) = &cl.test {
                        f(t);
                    }
                    for st in &cl.body {
                        visit_stmt_exprs(st, f);
                    }
                }
            }
            Stmt::While(s) => {
                f(&s.test);
                for st in s.body.iter().chain(s.orelse.iter()) {
                    visit_stmt_exprs(st, f);
                }
            }
            Stmt::For(s) => {
                f(&s.iter);
                for st in s.body.iter().chain(s.orelse.iter()) {
                    visit_stmt_exprs(st, f);
                }
            }
            Stmt::With(s) => {
                // `context_expr` is a use position (`with cm(i) as r:`); the
                // `optional_vars` target is a binding, so — like assignment
                // and loop targets — it is deliberately not visited (the
                // walker only inspects loads; `walk_names` ignores context).
                for item in &s.items {
                    f(&item.context_expr);
                }
                for st in &s.body {
                    visit_stmt_exprs(st, f);
                }
            }
            Stmt::Try(t) => {
                for st in t
                    .body
                    .iter()
                    .chain(t.orelse.iter())
                    .chain(t.finalbody.iter())
                {
                    visit_stmt_exprs(st, f);
                }
                for h in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    // The exception-type expression is a use (`except E(i):`);
                    // the bound name (`as e`) is a binding and is skipped.
                    if let Some(ty) = &h.type_ {
                        f(ty);
                    }
                    for st in &h.body {
                        visit_stmt_exprs(st, f);
                    }
                }
            }
            Stmt::Match(m) => {
                f(&m.subject);
                for case in &m.cases {
                    // The case guard is a use position; the pattern binds.
                    if let Some(guard) = &case.guard {
                        f(guard);
                    }
                    for st in &case.body {
                        visit_stmt_exprs(st, f);
                    }
                }
            }
            _ => {}
        }
    }

    fn walk_module(stmts: &[Stmt], path: &str, source: &str, diags: &mut Diagnostics) {
        for stmt in stmts {
            match stmt {
                Stmt::For(s) => {
                    let mut names: Vec<String> = Vec::new();
                    target_names(&s.target, &mut names);
                    if !names.is_empty() {
                        scan_stmts_for_closures(&s.body, &names, path, source, diags);
                    }
                    walk_module(&s.body, path, source, diags);
                    walk_module(&s.orelse, path, source, diags);
                }
                Stmt::FunctionDef(f) => walk_module(&f.body, path, source, diags),
                Stmt::ClassDef(c) => walk_module(&c.body, path, source, diags),
                Stmt::If(s) => {
                    walk_module(&s.body, path, source, diags);
                    for cl in &s.elif_else_clauses {
                        walk_module(&cl.body, path, source, diags);
                    }
                }
                Stmt::While(s) => {
                    walk_module(&s.body, path, source, diags);
                    walk_module(&s.orelse, path, source, diags);
                }
                Stmt::With(s) => walk_module(&s.body, path, source, diags),
                Stmt::Try(t) => {
                    walk_module(&t.body, path, source, diags);
                    for h in &t.handlers {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                        walk_module(&h.body, path, source, diags);
                    }
                    walk_module(&t.orelse, path, source, diags);
                    walk_module(&t.finalbody, path, source, diags);
                }
                Stmt::Match(m) => {
                    for case in &m.cases {
                        walk_module(&case.body, path, source, diags);
                    }
                }
                _ => {}
            }
        }
    }

    walk_module(&module.body, path, source, &mut diags);
    diags
}

/// Names that, when referenced inside a type annotation, indicate the
/// user is reaching for a deprecated `typing.<Name>` alias even though
/// the import would have been rejected by `tyc::typing_alias_deprecated`.
/// The returned suggestion mirrors the migration target.
fn typing_alias_suggestion(name: &str) -> Option<&'static str> {
    match name {
        "List" => Some("list"),
        "Dict" => Some("dict"),
        "Tuple" => Some("tuple"),
        "Set" => Some("set"),
        "FrozenSet" => Some("frozenset"),
        "Type" => Some("type"),
        // Optional[T] -> T? (Typhon's nullable sugar). The suggestion is
        // a single concrete sigil rather than a generic spelling because
        // the migration is mechanical.
        "Optional" => Some("T?"),
        // Union[A, B] -> A | B.
        "Union" => Some("A | B"),
        _ => None,
    }
}

/// `tyc::return_in_except_star` — `return` / `break` / `continue` inside an
/// `except*` handler body.
///
/// CPython rejects these at *compile* time
/// (`SyntaxError: 'break', 'continue' and 'return' cannot appear in an
/// except* block`) because an `except*` handler can run more than once —
/// once per matching subgroup — so a jump out of it has no defined meaning.
/// Typhon accepted them, type-checked clean, built successfully, and emitted
/// a `build/main.py` that CPython refused to import: a pipeline-escape defect
/// (`F7`).
///
/// The rule is replicated exactly, verified against CPython 3.13's
/// `compile()`:
///
/// * `return` is rejected anywhere lexically inside the handler body, at any
///   statement-nesting depth, **except** inside a nested `def` / `class`
///   (which open a new function scope, so their `return` binds there).
/// * `break` / `continue` are rejected on the same terms, **except** when
///   they are bound to a `for` / `while` declared *inside* the handler body.
///   A jump in that loop's `else:` clause targets the *outer* loop, so it is
///   still rejected — matching CPython.
/// * The `try` body, the `else:` clause and the `finally:` clause of the same
///   `try` statement are *not* part of the `except*` block, and are not
///   flagged.
///
/// Only ever fires on code the emitted Python could not compile, so it is a
/// narrowing on already-crashing programs (the v1.0.0-alpha.2 carve-out),
/// never on a program that ran correctly.
pub fn analyse_except_star_control_flow(
    module: &ModModule,
    path: &str,
    source: &str,
) -> Diagnostics {
    let mut diags = Diagnostics::new();
    walk_for_except_star(&module.body, path, source, &mut diags);
    diags
}

/// Find every `except*` handler in `body` (descending through every
/// statement that can nest another statement, including nested functions and
/// classes) and scan its handler bodies.
fn walk_for_except_star(body: &[Stmt], path: &str, source: &str, diags: &mut Diagnostics) {
    for stmt in body {
        match stmt {
            Stmt::FunctionDef(f) => walk_for_except_star(&f.body, path, source, diags),
            Stmt::ClassDef(c) => walk_for_except_star(&c.body, path, source, diags),
            Stmt::If(i) => {
                walk_for_except_star(&i.body, path, source, diags);
                for clause in &i.elif_else_clauses {
                    walk_for_except_star(&clause.body, path, source, diags);
                }
            }
            Stmt::While(w) => {
                walk_for_except_star(&w.body, path, source, diags);
                walk_for_except_star(&w.orelse, path, source, diags);
            }
            Stmt::For(f) => {
                walk_for_except_star(&f.body, path, source, diags);
                walk_for_except_star(&f.orelse, path, source, diags);
            }
            Stmt::With(w) => walk_for_except_star(&w.body, path, source, diags),
            Stmt::Match(m) => {
                for case in &m.cases {
                    walk_for_except_star(&case.body, path, source, diags);
                }
            }
            Stmt::Try(t) => {
                walk_for_except_star(&t.body, path, source, diags);
                walk_for_except_star(&t.orelse, path, source, diags);
                walk_for_except_star(&t.finalbody, path, source, diags);
                for h in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    if t.is_star {
                        // `in_loop = false`: no loop declared inside this
                        // handler body has been entered yet, so any
                        // `break`/`continue` here would target a loop outside
                        // the `except*` block.
                        scan_except_star_body(&h.body, false, path, source, diags);
                    }
                    // Still descend so a nested `except*` inside a plain
                    // handler (or inside another `except*`) is checked too.
                    walk_for_except_star(&h.body, path, source, diags);
                }
            }
            Stmt::Return(_)
            | Stmt::Delete(_)
            | Stmt::TypeAlias(_)
            | Stmt::Assign(_)
            | Stmt::AugAssign(_)
            | Stmt::AnnAssign(_)
            | Stmt::Raise(_)
            | Stmt::Assert(_)
            | Stmt::Import(_)
            | Stmt::ImportFrom(_)
            | Stmt::Global(_)
            | Stmt::Nonlocal(_)
            | Stmt::Expr(_)
            | Stmt::Pass(_)
            | Stmt::Break(_)
            | Stmt::Continue(_)
            | Stmt::IpyEscapeCommand(_) => {}
        }
    }
}

/// Report every `return` / `break` / `continue` in an `except*` handler body
/// that CPython would reject. `in_loop` tracks whether we are inside a
/// `for` / `while` **body** declared within the handler — the only place a
/// `break` / `continue` is legal.
fn scan_except_star_body(
    body: &[Stmt],
    in_loop: bool,
    path: &str,
    source: &str,
    diags: &mut Diagnostics,
) {
    use ruff_text_size::Ranged;
    for stmt in body {
        match stmt {
            Stmt::Return(r) => {
                push_except_star_error("return", r.range(), path, source, diags);
            }
            Stmt::Break(b) if !in_loop => {
                push_except_star_error("break", b.range(), path, source, diags);
            }
            Stmt::Continue(c) if !in_loop => {
                push_except_star_error("continue", c.range(), path, source, diags);
            }
            // A `break` / `continue` bound to a loop declared inside the
            // handler is legal — nothing to report, nothing to descend into.
            Stmt::Break(_) | Stmt::Continue(_) => {}
            // A nested `def` / `class` opens a new function scope: its
            // `return` binds there, and CPython accepts it.
            Stmt::FunctionDef(_) | Stmt::ClassDef(_) => {}
            Stmt::If(i) => {
                scan_except_star_body(&i.body, in_loop, path, source, diags);
                for clause in &i.elif_else_clauses {
                    scan_except_star_body(&clause.body, in_loop, path, source, diags);
                }
            }
            Stmt::While(w) => {
                scan_except_star_body(&w.body, true, path, source, diags);
                // A jump in a loop's `else:` clause targets the *enclosing*
                // loop, not this one (verified against CPython 3.13).
                scan_except_star_body(&w.orelse, in_loop, path, source, diags);
            }
            Stmt::For(f) => {
                scan_except_star_body(&f.body, true, path, source, diags);
                scan_except_star_body(&f.orelse, in_loop, path, source, diags);
            }
            Stmt::With(w) => scan_except_star_body(&w.body, in_loop, path, source, diags),
            Stmt::Match(m) => {
                for case in &m.cases {
                    scan_except_star_body(&case.body, in_loop, path, source, diags);
                }
            }
            Stmt::Try(t) => {
                scan_except_star_body(&t.body, in_loop, path, source, diags);
                scan_except_star_body(&t.orelse, in_loop, path, source, diags);
                scan_except_star_body(&t.finalbody, in_loop, path, source, diags);
                for h in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    scan_except_star_body(&h.body, in_loop, path, source, diags);
                }
            }
            Stmt::Delete(_)
            | Stmt::TypeAlias(_)
            | Stmt::Assign(_)
            | Stmt::AugAssign(_)
            | Stmt::AnnAssign(_)
            | Stmt::Raise(_)
            | Stmt::Assert(_)
            | Stmt::Import(_)
            | Stmt::ImportFrom(_)
            | Stmt::Global(_)
            | Stmt::Nonlocal(_)
            | Stmt::Expr(_)
            | Stmt::Pass(_)
            | Stmt::IpyEscapeCommand(_) => {}
        }
    }
}

fn push_except_star_error(
    keyword: &str,
    range: ruff_text_size::TextRange,
    path: &str,
    source: &str,
    diags: &mut Diagnostics,
) {
    let start = range.start().to_usize();
    let length = range.end().to_usize().saturating_sub(start).max(1);
    diags.push_error(TycError::return_in_except_star(
        keyword,
        path,
        source.to_owned(),
        start,
        length,
    ));
}

/// Walk every annotation expression in `module` and warn when it
/// references a deprecated `typing.<Name>` alias by bare name (`List`,
/// `Dict`, `Tuple`, `Set`, `FrozenSet`, `Type`, `Optional`, `Union`).
/// The annotation is silently accepted as a forward-reference name; the
/// warning surfaces the inconsistency so users migrate to the built-in
/// lowercase / sugar forms.
pub fn analyse_typing_alias_annotations(
    module: &ModModule,
    path: &str,
    source: &str,
) -> Diagnostics {
    let mut diags = Diagnostics::new();
    walk_annotation_stmts(&module.body, path, source, &mut diags);
    diags
}

fn walk_annotation_stmts(body: &[Stmt], path: &str, source: &str, diags: &mut Diagnostics) {
    for stmt in body {
        match stmt {
            Stmt::AnnAssign(a) => {
                walk_annotation_expr(&a.annotation, path, source, diags);
            }
            Stmt::FunctionDef(f) => {
                if let Some(ret) = f.returns.as_deref() {
                    walk_annotation_expr(ret, path, source, diags);
                }
                for param in f.parameters.posonlyargs.iter() {
                    if let Some(ann) = param.parameter.annotation.as_deref() {
                        walk_annotation_expr(ann, path, source, diags);
                    }
                }
                for param in f.parameters.args.iter() {
                    if let Some(ann) = param.parameter.annotation.as_deref() {
                        walk_annotation_expr(ann, path, source, diags);
                    }
                }
                for param in f.parameters.kwonlyargs.iter() {
                    if let Some(ann) = param.parameter.annotation.as_deref() {
                        walk_annotation_expr(ann, path, source, diags);
                    }
                }
                if let Some(va) = f.parameters.vararg.as_deref() {
                    if let Some(ann) = va.annotation.as_deref() {
                        walk_annotation_expr(ann, path, source, diags);
                    }
                }
                if let Some(kw) = f.parameters.kwarg.as_deref() {
                    if let Some(ann) = kw.annotation.as_deref() {
                        walk_annotation_expr(ann, path, source, diags);
                    }
                }
                walk_annotation_stmts(&f.body, path, source, diags);
            }
            Stmt::ClassDef(c) => walk_annotation_stmts(&c.body, path, source, diags),
            Stmt::If(i) => {
                walk_annotation_stmts(&i.body, path, source, diags);
                for clause in &i.elif_else_clauses {
                    walk_annotation_stmts(&clause.body, path, source, diags);
                }
            }
            Stmt::While(w) => {
                walk_annotation_stmts(&w.body, path, source, diags);
                walk_annotation_stmts(&w.orelse, path, source, diags);
            }
            Stmt::For(f) => {
                walk_annotation_stmts(&f.body, path, source, diags);
                walk_annotation_stmts(&f.orelse, path, source, diags);
            }
            Stmt::With(w) => walk_annotation_stmts(&w.body, path, source, diags),
            Stmt::Try(t) => {
                walk_annotation_stmts(&t.body, path, source, diags);
                walk_annotation_stmts(&t.orelse, path, source, diags);
                walk_annotation_stmts(&t.finalbody, path, source, diags);
                for h in &t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    walk_annotation_stmts(&h.body, path, source, diags);
                }
            }
            _ => {}
        }
    }
}

/// Recurse into an annotation expression and report every name reference
/// that looks like a `typing.<Name>` alias. Walks subscripts (`List[int]`),
/// unions, tuples, and `Optional[T]` recursively so nested usages like
/// `dict[str, Optional[int]]` are flagged on the inner `Optional`, not
/// silently passed through.
fn walk_annotation_expr(expr: &Expr, path: &str, source: &str, diags: &mut Diagnostics) {
    match expr {
        Expr::Name(n) => {
            if let Some(suggestion) = typing_alias_suggestion(n.id.as_str()) {
                let span_start = n.range.start().to_usize();
                let length = n.id.as_str().len();
                diags.push_warning(TycError::typing_alias_in_annotation(
                    n.id.as_str().to_owned(),
                    suggestion,
                    path,
                    source.to_owned(),
                    span_start,
                    length,
                ));
            }
        }
        Expr::Subscript(s) => {
            // `List[int]` / `Optional[int]` / `Union[int, str]` / nested
            // forms. Visit both the head (which produces the warning when
            // it's a typing alias) and the slice (so inner annotations
            // like `list[Optional[int]]` still surface).
            walk_annotation_expr(&s.value, path, source, diags);
            walk_annotation_expr(&s.slice, path, source, diags);
        }
        Expr::Tuple(t) => {
            for e in &t.elts {
                walk_annotation_expr(e, path, source, diags);
            }
        }
        Expr::BinOp(b) => {
            // `A | B` union sugar — recurse so a `List | None` still
            // warns on the deprecated `List`.
            walk_annotation_expr(&b.left, path, source, diags);
            walk_annotation_expr(&b.right, path, source, diags);
        }
        Expr::Attribute(a) => {
            // `typing.List[int]` — only warn when the head is exactly
            // `typing`; any other `foo.List` is the user's own attribute.
            if let Expr::Name(head) = a.value.as_ref() {
                if head.id.as_str() == "typing" {
                    if let Some(suggestion) = typing_alias_suggestion(a.attr.as_str()) {
                        let span_start = a.range.start().to_usize();
                        let length = a.range.len().to_usize().max(1);
                        diags.push_warning(TycError::typing_alias_in_annotation(
                            a.attr.as_str().to_owned(),
                            suggestion,
                            path,
                            source.to_owned(),
                            span_start,
                            length,
                        ));
                    }
                }
            }
        }
        _ => {}
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod lint_tests {
    use super::*;
    use miette::Diagnostic as _;
    use tyc_syntax::preprocess::preprocess;

    /// Collect the `tyc::…` codes of a diagnostic set's warnings.
    fn warning_codes(diags: &Diagnostics) -> Vec<String> {
        diags
            .warnings()
            .iter()
            .filter_map(|w| w.code().map(|c| c.to_string()))
            .collect()
    }

    fn parse(src: &str) -> ModModule {
        let prep = preprocess(src);
        tyc_syntax::parse_module(&prep.python_source)
            .expect("parse failed")
            .into_syntax()
    }

    // ── Shared editor / CLI advisory aggregator ─────────────────────────────

    #[test]
    fn editor_lint_diagnostics_bundles_gather_and_lints() {
        // One independent-await run (gather advice) plus a mutable-default
        // param (lint) — the aggregator should surface both in one pass.
        let src = "\
async def fetch_a() -> int:
    return 1
async def fetch_b() -> int:
    return 2
async def load() -> int:
    let a = await fetch_a()
    let b = await fetch_b()
    return a + b
def f(xs: list[int] = []) -> int:
    return len(xs)
";
        let prep = preprocess(src);
        let module = parse(src);
        let diags = editor_lint_diagnostics(
            &module,
            "x.ty",
            &prep.python_source,
            LintOptions::default(),
            &PerfLintContext::default(),
        );
        let codes = warning_codes(&diags);
        assert!(
            codes.iter().any(|c| c.contains("gather_opportunity")),
            "expected gather_opportunity advice; got {codes:?}"
        );
        assert!(
            codes.iter().any(|c| c.contains("mutable_default_param")),
            "expected mutable_default_param lint; got {codes:?}"
        );
    }

    #[test]
    fn editor_lint_diagnostics_respects_suggest_gather_off() {
        let src = "\
async def fetch_a() -> int:
    return 1
async def fetch_b() -> int:
    return 2
async def load() -> int:
    let a = await fetch_a()
    let b = await fetch_b()
    return a + b
";
        let prep = preprocess(src);
        let module = parse(src);
        let opts = LintOptions {
            suggest_gather: false,
            ..LintOptions::default()
        };
        let diags = editor_lint_diagnostics(
            &module,
            "x.ty",
            &prep.python_source,
            opts,
            &PerfLintContext::default(),
        );
        let has_gather = warning_codes(&diags)
            .iter()
            .any(|c| c.contains("gather_opportunity"));
        assert!(
            !has_gather,
            "suggest_gather = false must silence the gather nudge"
        );
    }

    #[test]
    fn editor_lint_diagnostics_respects_suggest_perf_off() {
        // A `sorted(...)[0]` (perf_sorted_first) and a module-level-only
        // third-party import (lazy_import_opportunity) are both present; with
        // `suggest_perf = false` neither surfaces.
        let src = "\
import numpy as np
def first(xs: list[int]) -> int:
    return sorted(xs)[0]
def use_np() -> object:
    return np.array([1])
";
        let prep = preprocess(src);
        let module = parse(src);
        let on = editor_lint_diagnostics(
            &module,
            "x.ty",
            &prep.python_source,
            LintOptions::default(),
            &PerfLintContext::default(),
        );
        assert!(
            warning_codes(&on).iter().any(|c| c.contains("perf_")),
            "sanity: perf lints fire by default; got {:?}",
            warning_codes(&on)
        );
        let opts = LintOptions {
            suggest_perf: false,
            ..LintOptions::default()
        };
        let off = editor_lint_diagnostics(
            &module,
            "x.ty",
            &prep.python_source,
            opts,
            &PerfLintContext::default(),
        );
        let perf_hits: Vec<String> = warning_codes(&off)
            .into_iter()
            .filter(|c| c.contains("perf_") || c.contains("lazy_import_opportunity"))
            .collect();
        assert!(
            perf_hits.is_empty(),
            "suggest_perf = false must silence the whole perf family; got {perf_hits:?}"
        );
    }

    // ── Finding #3: empty_collection_no_annotation ──────────────────────────

    #[test]
    fn empty_list_no_annotation_warns() {
        // `let xs = []` — must warn.
        let prep = preprocess("let xs = []\n");
        let module = tyc_syntax::parse_module(&prep.python_source)
            .expect("parse failed")
            .into_syntax();
        let diags = analyse_empty_collection_bindings(&module, "x.ty", &prep.python_source);
        assert_eq!(
            diags.warnings().len(),
            1,
            "let xs = [] must warn; got {:?}",
            diags.warnings()
        );
    }

    #[test]
    fn empty_dict_no_annotation_warns() {
        let prep = preprocess("let d = {}\n");
        let module = tyc_syntax::parse_module(&prep.python_source)
            .expect("parse failed")
            .into_syntax();
        let diags = analyse_empty_collection_bindings(&module, "x.ty", &prep.python_source);
        assert_eq!(diags.warnings().len(), 1);
    }

    #[test]
    fn empty_set_call_no_annotation_warns() {
        let prep = preprocess("let s = set()\n");
        let module = tyc_syntax::parse_module(&prep.python_source)
            .expect("parse failed")
            .into_syntax();
        let diags = analyse_empty_collection_bindings(&module, "x.ty", &prep.python_source);
        assert_eq!(diags.warnings().len(), 1);
    }

    #[test]
    fn empty_list_with_annotation_silent() {
        // `let xs: list[int] = []` — annotation pins the element type, no warn.
        let prep = preprocess("let xs: list[int] = []\n");
        let module = tyc_syntax::parse_module(&prep.python_source)
            .expect("parse failed")
            .into_syntax();
        let diags = analyse_empty_collection_bindings(&module, "x.ty", &prep.python_source);
        assert!(
            diags.warnings().is_empty(),
            "annotated empty literal must NOT warn; got {:?}",
            diags.warnings()
        );
    }

    #[test]
    fn nonempty_list_silent() {
        let prep = preprocess("let xs = [1, 2, 3]\n");
        let module = tyc_syntax::parse_module(&prep.python_source)
            .expect("parse failed")
            .into_syntax();
        let diags = analyse_empty_collection_bindings(&module, "x.ty", &prep.python_source);
        assert!(diags.warnings().is_empty());
    }

    // ── Finding #4: typing_alias_in_annotation ──────────────────────────────

    #[test]
    fn list_in_annotation_warns() {
        let src = "def f(xs: List[int]) -> None:\n    pass\n";
        let module = parse(src);
        let diags =
            analyse_typing_alias_annotations(&module, "x.ty", &preprocess(src).python_source);
        assert_eq!(
            diags.warnings().len(),
            1,
            "`List[int]` annotation must warn; got {:?}",
            diags.warnings()
        );
    }

    #[test]
    fn optional_in_annotation_warns() {
        let src = "def f(x: Optional[int]) -> None:\n    pass\n";
        let module = parse(src);
        let diags =
            analyse_typing_alias_annotations(&module, "x.ty", &preprocess(src).python_source);
        assert_eq!(diags.warnings().len(), 1);
    }

    #[test]
    fn dict_in_annotation_warns() {
        let src = "def f(x: Dict[str, int]) -> None:\n    pass\n";
        let module = parse(src);
        let diags =
            analyse_typing_alias_annotations(&module, "x.ty", &preprocess(src).python_source);
        assert_eq!(diags.warnings().len(), 1);
    }

    #[test]
    fn union_in_annotation_warns() {
        let src = "def f(x: Union[int, str]) -> None:\n    pass\n";
        let module = parse(src);
        let diags =
            analyse_typing_alias_annotations(&module, "x.ty", &preprocess(src).python_source);
        assert_eq!(diags.warnings().len(), 1);
    }

    #[test]
    fn nested_optional_in_annotation_warns() {
        let src = "def f(x: list[Optional[int]]) -> None:\n    pass\n";
        let module = parse(src);
        let diags =
            analyse_typing_alias_annotations(&module, "x.ty", &preprocess(src).python_source);
        assert_eq!(
            diags.warnings().len(),
            1,
            "nested Optional must still warn; got {:?}",
            diags.warnings()
        );
    }

    #[test]
    fn return_type_alias_warns() {
        let src = "def f() -> List[int]:\n    return []\n";
        let module = parse(src);
        let diags =
            analyse_typing_alias_annotations(&module, "x.ty", &preprocess(src).python_source);
        assert_eq!(diags.warnings().len(), 1);
    }

    #[test]
    fn ann_assign_with_typing_alias_warns() {
        let src = "let xs: List[int] = [1, 2]\n";
        let prep = preprocess(src);
        let module = tyc_syntax::parse_module(&prep.python_source)
            .expect("parse failed")
            .into_syntax();
        let diags = analyse_typing_alias_annotations(&module, "x.ty", &prep.python_source);
        assert_eq!(diags.warnings().len(), 1);
    }

    #[test]
    fn lowercase_annotation_silent() {
        // The recommended form must not warn — it's exactly what we want.
        let src = "def f(xs: list[int]) -> None:\n    pass\n";
        let module = parse(src);
        let diags =
            analyse_typing_alias_annotations(&module, "x.ty", &preprocess(src).python_source);
        assert!(
            diags.warnings().is_empty(),
            "lowercase annotation must be silent; got {:?}",
            diags.warnings()
        );
    }

    #[test]
    fn typing_dot_alias_in_annotation_warns() {
        // `typing.List[int]` — fully-qualified form must also warn.
        let src = "def f(xs: typing.List[int]) -> None:\n    pass\n";
        let module = parse(src);
        let diags =
            analyse_typing_alias_annotations(&module, "x.ty", &preprocess(src).python_source);
        assert_eq!(diags.warnings().len(), 1);
    }

    // ── Finding #10: contains_secret_literal (inline string form) ───────────

    #[test]
    fn secret_literal_fires_on_string_assign() {
        let module = parse("API_TOKEN = \"abc\"\n");
        let diags =
            analyse_secret_literal_bindings(&module, "x.ty", "API_TOKEN = \"abc\"\n", false);
        assert_eq!(
            diags.warnings().len(),
            1,
            "API_TOKEN = string literal must warn; got {:?}",
            diags.warnings()
        );
    }

    #[test]
    fn secret_literal_fires_on_password_and_pwd() {
        let src = "DB_PASSWORD = \"secret\"\nDB_PWD = \"abc\"\n";
        let module = parse(src);
        let diags = analyse_secret_literal_bindings(&module, "x.ty", src, false);
        assert_eq!(diags.warnings().len(), 2);
    }

    #[test]
    fn secret_literal_fires_on_openai_api_key() {
        let src = "OPENAI_API_KEY = \"sk-foo\"\n";
        let module = parse(src);
        let diags = analyse_secret_literal_bindings(&module, "x.ty", src, false);
        assert_eq!(diags.warnings().len(), 1);
    }

    #[test]
    fn secret_literal_fires_on_my_secret() {
        let src = "MY_SECRET = \"abc\"\n";
        let module = parse(src);
        let diags = analyse_secret_literal_bindings(&module, "x.ty", src, false);
        assert_eq!(diags.warnings().len(), 1);
    }

    #[test]
    fn secret_literal_fires_on_embedded_words() {
        let srcs = [
            "API_KEY_FOO = \"sk-foo\"\n",
            "FOO_API_KEY_BAR = \"sk-foo\"\n",
            "KEY_APIKEY = \"sk-foo\"\n",
            "myTokenValue = \"sk-foo\"\n",
            "APIKEY = \"123\"\n",
            "APITOKEN = \"abc\"\n",
            "APISECRET = \"abc\"\n",
            "TOKEN123 = \"abc\"\n",
            "foo123TOKEN = \"abc\"\n",
            "my123TOKEN = \"abc\"\n",
            "TOKENString = \"abc\"\n",
            "dbPASSWORDString = \"abc\"\n",
            "PRIVKEY = \"abc\"\n",
            "SSH_PRIVKEY = \"abc\"\n",
            "PRIVKEY_PEM = \"abc\"\n",
            "AUTHORIZATION = \"abc\"\n",
            "SESSION_COOKIE = \"abc\"\n",
            "WEBHOOK_URL = \"abc\"\n",
            "AWS_CREDENTIALS = \"abc\"\n",
            "DATABASE_DSN = \"abc\"\n",
            "SIGNING_KEY = \"abc\"\n",
            "DBPASSWORD = \"abc\"\n",
            "DBSECRET = \"abc\"\n",
            "DBPASS = \"abc\"\n",
            "DBPWD = \"abc\"\n",
            "DB_PASS = \"abc\"\n",
            "DB_PWD = \"abc\"\n",
            "JWTTOKEN = \"abc\"\n",
            "JWTSECRET = \"abc\"\n",
            "ACCESSTOKEN = \"abc\"\n",
            "APPSECRET = \"abc\"\n",
            "ACCESS_TOKEN = \"abc\"\n",
            "AUTH_TOKEN = \"abc\"\n",
            "SECRETKEY = \"abc\"\n",
            "SSHKEY = \"abc\"\n",
        ];
        for src in srcs {
            let module = parse(src);
            let diags = analyse_secret_literal_bindings(&module, "x.ty", src, false);
            assert_eq!(diags.warnings().len(), 1, "Failed to flag {src:?}");
        }
    }

    #[test]
    fn secret_literal_silent_on_env_lookup() {
        // The whole point of the lint: env-driven RHS should NOT warn.
        let src = "import os\nAPI_TOKEN = os.getenv(\"API_TOKEN\")\n";
        let module = parse(src);
        let diags = analyse_secret_literal_bindings(&module, "x.ty", src, false);
        assert!(
            diags.warnings().is_empty(),
            "env-driven RHS must not warn; got {:?}",
            diags.warnings()
        );
    }

    #[test]
    fn secret_literal_silent_on_unrelated_string_name() {
        // A regular `let username = "x"` must stay silent.
        let src = "username = \"alice\"\nMONKEY = \"chimp\"\n";
        let module = parse(src);
        let diags = analyse_secret_literal_bindings(&module, "x.ty", src, false);
        assert!(
            diags.warnings().is_empty(),
            "unrelated binding name must not warn; got {:?}",
            diags.warnings()
        );
    }

    #[test]
    fn secret_literal_fires_on_annassign_let() {
        // `let TOKEN: str = "abc"` — AnnAssign path.
        let src = "let TOKEN: str = \"abc\"\n";
        let prep = preprocess(src);
        let module = tyc_syntax::parse_module(&prep.python_source)
            .expect("parse failed")
            .into_syntax();
        let diags = analyse_secret_literal_bindings(&module, "x.ty", &prep.python_source, false);
        assert_eq!(
            diags.warnings().len(),
            1,
            "annotated let TOKEN must warn; got {:?}",
            diags.warnings()
        );
    }
}

#[cfg(test)]
mod purity_tests {
    use super::*;

    fn analyse(src: &str) -> Vec<PurityFinding> {
        let module = tyc_syntax::parse_module(src)
            .expect("parse failed")
            .into_syntax();
        analyse_purity(&module, false)
    }

    #[test]
    fn plain_pure_function_passes() {
        let findings = analyse("@pure\ndef add(a: int, b: int) -> int:\n    return a + b\n");
        assert_eq!(findings.len(), 1);
        assert!(findings[0].declared_pure);
        assert!(
            findings[0].violation.is_none(),
            "{:?}",
            findings[0].violation
        );
        assert!(!findings[0].memoise);
    }

    #[test]
    fn memo_decorator_sets_memoise() {
        let findings = analyse("@memo\ndef add(a: int, b: int) -> int:\n    return a + b\n");
        assert_eq!(findings.len(), 1);
        assert!(findings[0].memoise);
    }

    #[test]
    fn pure_memo_true_sets_memoise() {
        let findings =
            analyse("@pure(memo=True)\ndef add(a: int, b: int) -> int:\n    return a + b\n");
        assert_eq!(findings.len(), 1);
        assert!(findings[0].memoise);
    }

    #[test]
    fn pure_async_is_rejected() {
        let findings = analyse("@pure\nasync def fetch(a: int) -> int:\n    return a\n");
        assert_eq!(findings.len(), 1);
        let reason = findings[0].violation.as_ref().expect("expected violation");
        assert!(reason.contains("async"));
    }

    #[test]
    fn pure_raises_rejected() {
        let findings = analyse("@pure\ndef bad(x: int) -> int:\n    raise ValueError(\"no\")\n");
        assert_eq!(findings.len(), 1);
        let reason = findings[0].violation.as_ref().expect("expected violation");
        assert!(reason.contains("raise"));
    }

    #[test]
    fn pure_io_call_rejected() {
        let findings = analyse("@pure\ndef bad(x: int) -> int:\n    print(x)\n    return x\n");
        assert_eq!(findings.len(), 1);
        let reason = findings[0].violation.as_ref().expect("expected violation");
        assert!(reason.contains("print") || reason.contains("I/O"));
    }

    #[test]
    fn pure_random_call_rejected() {
        let findings = analyse("@pure\ndef pick() -> int:\n    return random.randint(0, 10)\n");
        assert_eq!(findings.len(), 1);
        let reason = findings[0].violation.as_ref().expect("expected violation");
        assert!(reason.contains("entropy") || reason.contains("non-deterministic"));
    }

    #[test]
    fn pure_clock_call_rejected() {
        let findings = analyse("@pure\ndef stamp() -> float:\n    return time.time()\n");
        assert_eq!(findings.len(), 1);
        let reason = findings[0].violation.as_ref().expect("expected violation");
        assert!(reason.contains("clock") || reason.contains("non-deterministic"));
    }

    #[test]
    fn non_pure_function_skipped() {
        let findings = analyse("def add(a: int, b: int) -> int:\n    return a + b\n");
        assert!(findings.is_empty(), "non-@pure fns should not be reported");
    }

    #[test]
    fn auto_memoise_does_not_force_purity_diagnostics() {
        // An impure helper without an explicit `@pure` decorator should not
        // become a hard build error just because the project opted in to
        // `[strictness] auto-memoise = true`. The auto-memoise flag is a
        // silent best-effort: a finding is recorded (so the memoise pass
        // knows to SKIP this one) but the diagnostic stage drops it.
        let src = "def fetch(url: str) -> str:\n    return open(url).read()\n";
        let module = tyc_syntax::parse_module(src).unwrap().into_syntax();
        let findings = analyse_purity(&module, /*auto_memoise=*/ true);
        assert_eq!(findings.len(), 1);
        // Not declared pure → never a hard error.
        assert!(!findings[0].declared_pure);
        // The purity check still ran and found the I/O call so the desugarer
        // knows not to inject `@functools.cache`.
        assert!(findings[0].violation.is_some());
        // And the diagnostic stage drops it because declared_pure is false.
        let diags = purity_diagnostics(&findings, "<test>", src);
        assert!(
            !diags.has_errors(),
            "auto-memoise must not surface hard errors: {:?}",
            diags.errors()
        );
    }

    #[test]
    fn auto_memoise_caches_passable_function() {
        // Counterpart to the above — a passable function should produce a
        // finding with `memoise = true && violation = None`, which the
        // build pipeline turns into a `@functools.cache` injection.
        let src = "def add(a: int, b: int) -> int:\n    return a + b\n";
        let module = tyc_syntax::parse_module(src).unwrap().into_syntax();
        let findings = analyse_purity(&module, /*auto_memoise=*/ true);
        assert_eq!(findings.len(), 1);
        assert!(!findings[0].declared_pure);
        assert!(findings[0].memoise);
        assert!(findings[0].violation.is_none());
    }

    // ── Phase 3 review-feedback regressions ────────────────────────────────

    #[test]
    fn pure_local_assignment_allowed() {
        // Bare-name assignment inside a pure body is local by Python scoping
        // (because `global` is already forbidden), so it must not be flagged
        // even when the same name is also bound at module scope.
        let findings = analyse(
            "PORT = 8080\n\n@pure\ndef add(a: int, b: int) -> int:\n    PORT = a + b\n    return PORT\n",
        );
        assert_eq!(findings.len(), 1);
        assert!(
            findings[0].violation.is_none(),
            "local rebinding of a name that happens to exist at module scope is not a side effect: {:?}",
            findings[0].violation
        );
    }

    #[test]
    fn pure_attribute_mutation_on_module_state_rejected() {
        // Mutating a module-level binding through `.attr` or `[idx]` IS a
        // side effect, even though bare-name assignments are local.
        let findings = analyse(
            "CACHE = {}\n\n@pure\ndef remember(k: int, v: int) -> int:\n    CACHE[k] = v\n    return v\n",
        );
        assert_eq!(findings.len(), 1);
        let reason = findings[0]
            .violation
            .as_ref()
            .expect("expected mutation violation");
        assert!(reason.contains("module-level state"), "got: {reason}");
    }

    #[test]
    fn pure_transitive_call_to_pure_helper_allowed() {
        let findings = analyse(
            "@pure\ndef double(x: int) -> int:\n    return x + x\n\n@pure\ndef quad(x: int) -> int:\n    return double(double(x))\n",
        );
        assert_eq!(findings.len(), 2);
        for f in &findings {
            assert!(f.violation.is_none(), "{:?}", f.violation);
        }
    }

    #[test]
    fn pure_transitive_call_to_impure_helper_rejected() {
        let findings = analyse(
            "def shout(s: str) -> str:\n    print(s)\n    return s\n\n@pure\ndef greet(name: str) -> str:\n    return shout(name)\n",
        );
        let greet = findings
            .iter()
            .find(|f| f.name == "greet")
            .expect("greet should be analysed");
        let reason = greet
            .violation
            .as_ref()
            .expect("expected transitive impurity");
        assert!(reason.contains("shout"), "got: {reason}");
    }

    #[test]
    fn pure_call_to_unknown_callable_rejected() {
        // Unknown identifiers (likely imported) can't be proven pure, so
        // reject them conservatively.
        let findings = analyse("@pure\ndef wrap(x: int) -> int:\n    return mystery(x)\n");
        let reason = findings[0]
            .violation
            .as_ref()
            .expect("expected unknown-callee rejection");
        assert!(reason.contains("mystery"), "got: {reason}");
    }

    #[test]
    fn pure_call_to_class_constructor_allowed() {
        let findings = analyse(
            "class Box:\n    value: int\n\n@pure\ndef wrap(x: int) -> Box:\n    return Box(x)\n",
        );
        let wrap = findings
            .iter()
            .find(|f| f.name == "wrap")
            .expect("wrap should be analysed");
        assert!(
            wrap.violation.is_none(),
            "class constructors are pure-callable: {:?}",
            wrap.violation
        );
    }

    #[test]
    fn memo_with_unhashable_param_rejected() {
        // `@memo` (or `@pure(memo=True)`) backs the cache with a dict keyed
        // on the args; a `list[int]` parameter would crash at runtime with
        // `TypeError: unhashable type`. Reject the annotation up front.
        let findings = analyse("@memo\ndef sum_all(xs: list[int]) -> int:\n    return 0\n");
        let reason = findings[0]
            .violation
            .as_ref()
            .expect("expected hashability violation");
        assert!(reason.contains("unhashable"), "got: {reason}");
    }

    // ── Beta hardening: attribute callees, module state, cache safety ──────

    fn analyse_auto(src: &str) -> Vec<PurityFinding> {
        let module = tyc_syntax::parse_module(src)
            .expect("parse failed")
            .into_syntax();
        analyse_purity(&module, /*auto_memoise=*/ true)
    }

    #[test]
    fn clock_reads_through_attribute_callees_are_rejected() {
        for src in [
            "import datetime\n@pure\ndef stamp() -> str:\n    return datetime.datetime.now().isoformat()\n",
            "from datetime import datetime\n@pure\ndef stamp() -> str:\n    return datetime.now().isoformat()\n",
            "import datetime\n@pure\ndef today() -> str:\n    return datetime.date.today().isoformat()\n",
            "import time as t\n@pure\ndef tick() -> float:\n    return t.perf_counter()\n",
        ] {
            let f = analyse(src);
            assert!(f[0].violation.is_some(), "must be rejected: {src}");
        }
    }

    #[test]
    fn logger_calls_and_io_methods_are_rejected() {
        for src in [
            "import logging\nlogger: logging.Logger = logging.getLogger(\"t\")\n@pure\ndef f(x: int) -> int:\n    logger.warning(\"x=%d\", x)\n    return x\n",
            "import pathlib\n@pure\ndef read(p: str) -> str:\n    return pathlib.Path(p).read_text()\n",
            "from typing import Iterator\n@pure\ndef take(it: Iterator[int]) -> int:\n    return next(it)\n",
            "@pure\ndef f(p: str) -> str:\n    with open(p) as fh:\n        return fh.name\n",
        ] {
            let f = analyse(src);
            assert!(f[0].violation.is_some(), "must be rejected: {src}");
        }
    }

    #[test]
    fn mutating_module_state_or_arguments_through_methods_is_rejected() {
        for src in [
            "REGISTRY: list[int] = []\n@pure\ndef register(x: int) -> int:\n    REGISTRY.append(x)\n    return len(REGISTRY)\n",
            "class Counter:\n    n: int = 0\n\n@pure\ndef bump(c: Counter) -> int:\n    c.n = c.n + 1\n    return c.n\n",
            "class Bag:\n    items: tuple[int, ...] = ()\n\n@pure\ndef put(b: Bag, x: int) -> int:\n    b.items.append(x)\n    return x\n",
        ] {
            let f = analyse(src);
            assert!(f[0].violation.is_some(), "must be rejected: {src}");
        }
    }

    #[test]
    fn reading_mutable_module_state_is_rejected() {
        for src in [
            "mut COUNTER: int = 0\n\n@pure\ndef offset(x: int) -> int:\n    return x + COUNTER\n",
            "COUNTER: int = 0\n\ndef tick() -> None:\n    global COUNTER\n    COUNTER += 1\n\n@pure\ndef offset(x: int) -> int:\n    return x + COUNTER\n",
        ] {
            let f = analyse(src);
            let offset = f.iter().find(|f| f.name == "offset").expect("offset finding");
            assert!(offset.violation.is_some(), "must be rejected: {src}");
        }
    }

    #[test]
    fn immutable_module_constants_and_pure_stdlib_calls_are_provably_pure() {
        let src = "\
import math
import json
from decimal import Decimal
SEP: str = \", \"
LIMIT: int = 10

@pure
def f(xs: tuple[int, ...], s: str) -> str:
    parts = [str(x) for x in xs]
    out: list[str] = []
    out.append(s.upper())
    return SEP.join(parts) + json.dumps(math.sqrt(LIMIT)) + str(Decimal(\"1.5\")) + \", \".join(out)
";
        let f = analyse(src);
        assert!(f[0].violation.is_none(), "{:?}", f[0].violation);
        assert!(f[0].unproven.is_none(), "{:?}", f[0].unproven);
        assert!(f[0].is_provably_pure());
    }

    #[test]
    fn unknown_method_calls_are_unproven_not_violations() {
        for src in [
            "class Point:\n    x: float\n\n@pure\ndef norm(p: Point) -> float:\n    return p.length()\n",
            "import numpy as np\n@pure\ndef f(x: float) -> float:\n    return np.sqrt(x)\n",
            "TABLE: dict[str, int] = {}\n@pure\ndef look(k: str) -> int:\n    return TABLE.get(k, 0)\n",
        ] {
            let f = analyse(src);
            assert!(f[0].violation.is_none(), "{src}: {:?}", f[0].violation);
            assert!(f[0].unproven.is_some(), "{src} must be unproven");
            assert!(!f[0].is_provably_pure());
        }
    }

    #[test]
    fn auto_memoise_requires_provable_purity_and_cache_safe_signatures() {
        let make =
            analyse_auto("def make(n: int) -> list[int]:\n    return [i for i in range(n)]\n");
        assert!(
            make[0].is_provably_pure(),
            "{:?} {:?}",
            make[0].violation,
            make[0].unproven
        );
        assert!(make[0].unshareable_return.is_some());
        assert!(!make[0].auto_cacheable());

        let evens = analyse_auto(
            "from typing import Iterator\ndef evens(n: int) -> Iterator[int]:\n    return filter(lambda v: v % 2 == 0, range(n))\n",
        );
        assert!(evens[0].unshareable_return.is_some());
        assert!(!evens[0].auto_cacheable());

        let scale = analyse_auto(
            "class Point:\n    x: float\n\ndef scale(p: Point, k: float) -> float:\n    return p.x * k\n",
        );
        let scale = scale.iter().find(|f| f.name == "scale").unwrap();
        assert!(
            scale.is_provably_pure(),
            "{:?} {:?}",
            scale.violation,
            scale.unproven
        );
        assert!(scale.uncacheable_params.is_some());
        assert!(!scale.auto_cacheable());

        let unproven =
            analyse_auto("import mylib\ndef area(w: float) -> float:\n    return mylib.area(w)\n");
        assert!(unproven[0].violation.is_none());
        assert!(!unproven[0].auto_cacheable());
        assert!(!unproven[0].callable_as_pure());

        let add = analyse_auto("def add(a: int, b: int) -> int:\n    return a + b\n");
        assert!(add[0].auto_cacheable());
        assert!(add[0].callable_as_pure());
    }

    #[test]
    fn frozen_classes_are_cache_safe_through_analyse_purity_with() {
        let src = "class Point:\n    x: float\n\ndef scale(p: Point, k: float) -> Point:\n    return Point(x=p.x * k)\n";
        let module = tyc_syntax::parse_module(src).unwrap().into_syntax();
        let plain = analyse_purity(&module, true);
        let scale = plain.iter().find(|f| f.name == "scale").unwrap();
        assert!(!scale.auto_cacheable());
        let frozen: HashSet<String> = ["Point".to_owned()].into_iter().collect();
        let with = analyse_purity_with(&module, true, &frozen);
        let scale = with.iter().find(|f| f.name == "scale").unwrap();
        assert!(
            scale.auto_cacheable(),
            "{:?} {:?} {:?} {:?}",
            scale.violation,
            scale.unproven,
            scale.unshareable_return,
            scale.uncacheable_params
        );
    }

    #[test]
    fn explicit_pure_still_trusts_unproven_calls_but_optimiser_does_not() {
        // An `@pure` author calling into an unknown module gets no error (as
        // before), yet the function is not handed to the optimiser as pure.
        let f = analyse(
            "import mylib\n@pure\ndef area(w: float) -> float:\n    return mylib.area(w)\n",
        );
        assert!(f[0].declared_pure);
        assert!(f[0].violation.is_none());
        assert!(f[0].unproven.is_some());
        assert!(
            f[0].callable_as_pure(),
            "explicit @pure is trusted for the callee set"
        );
        assert!(!f[0].is_provably_pure());
    }

    #[test]
    fn fresh_locals_may_be_mutated_in_place() {
        for src in [
            "@pure\ndef f(n: int) -> tuple[int, ...]:\n    out: list[int] = []\n    for i in range(n):\n        out.append(i)\n    return tuple(out)\n",
            "@pure\ndef g(xs: tuple[int, ...]) -> int:\n    ys = list(xs)\n    ys.sort()\n    return ys[0]\n",
        ] {
            let f = analyse(src);
            assert!(f[0].is_provably_pure(), "{src}: {:?} {:?}", f[0].violation, f[0].unproven);
        }
        // An aliased local is not fresh.
        let f = analyse("@pure\ndef h(xs: tuple[int, ...], ys: Bag) -> int:\n    zs = ys\n    zs.add(1)\n    return 0\n");
        assert!(f[0].violation.is_none());
        assert!(f[0].unproven.is_some());
    }

    #[test]
    fn class_names_at_marker_starts_matches_decorated_classes() {
        let src = "@dataclass\nclass A:\n    x: int\n\nclass B:\n    y: int\n";
        let module = tyc_syntax::parse_module(src).unwrap().into_syntax();
        let a_line = src.find("class A").unwrap() as u32;
        let b_line = src.find("class B").unwrap() as u32;
        assert_eq!(
            class_names_at_marker_starts(&module, &[a_line]),
            ["A".to_owned()].into_iter().collect()
        );
        assert_eq!(
            class_names_at_marker_starts(&module, &[b_line]),
            ["B".to_owned()].into_iter().collect()
        );
        // A marker past the class name (the body line) covers nothing.
        let body_line = src.find("    y: int").unwrap() as u32;
        assert!(class_names_at_marker_starts(&module, &[body_line]).is_empty());
    }
}

#[cfg(test)]
mod except_star_tests {
    use super::*;

    /// Collect the offending keywords `analyse_except_star_control_flow`
    /// reports, in source order.
    fn offenders(src: &str) -> Vec<String> {
        let module = tyc_syntax::parse_module(src)
            .expect("parse failed")
            .into_syntax();
        let diags = analyse_except_star_control_flow(&module, "t.py", src);
        diags
            .errors()
            .iter()
            .map(|e| {
                // The message is "`return` cannot appear in an `except*` block".
                let text = e.to_string();
                text.split('`').nth(1).unwrap_or_default().to_owned()
            })
            .collect()
    }

    // Every case below was verified against CPython 3.13 `compile()`: the
    // ones asserted to fire are exactly the ones that raise
    // `SyntaxError: 'break', 'continue' and 'return' cannot appear in an
    // except* block`, and the ones asserted clean compile successfully.

    #[test]
    fn bare_return_in_except_star_is_rejected() {
        assert_eq!(
            offenders(
                "def f():\n    try:\n        pass\n    except* ValueError:\n        return 1\n"
            ),
            vec!["return"]
        );
    }

    #[test]
    fn return_nested_in_compound_statements_is_rejected() {
        // CPython rejects a `return` at any statement depth inside the
        // handler — `if`, `for`, `while`, `with`, `match`, and a nested
        // `try`'s `finally` all still count as "inside the except* block".
        for src in [
            "def f():\n    try:\n        pass\n    except* ValueError:\n        if 1:\n            return 1\n",
            "def f():\n    try:\n        pass\n    except* ValueError:\n        for i in range(3):\n            return 1\n",
            "def f():\n    try:\n        pass\n    except* ValueError:\n        while 1:\n            return 1\n",
            "def f():\n    try:\n        pass\n    except* ValueError:\n        with open('x'):\n            return 1\n",
            "def f(x):\n    try:\n        pass\n    except* ValueError:\n        match x:\n            case 1:\n                return 1\n",
            "def f():\n    try:\n        pass\n    except* ValueError:\n        try:\n            pass\n        finally:\n            return 1\n",
        ] {
            assert_eq!(offenders(src), vec!["return"], "src: {src}");
        }
    }

    #[test]
    fn break_and_continue_bound_outside_are_rejected() {
        assert_eq!(
            offenders(
                "def f():\n    for j in range(2):\n        try:\n            pass\n        except* ValueError:\n            break\n"
            ),
            vec!["break"]
        );
        assert_eq!(
            offenders(
                "def f():\n    for j in range(2):\n        try:\n            pass\n        except* ValueError:\n            continue\n"
            ),
            vec!["continue"]
        );
    }

    #[test]
    fn break_bound_to_a_loop_inside_the_handler_is_allowed() {
        // CPython accepts this: the jump target is declared inside the
        // handler, so it never leaves the `except*` block.
        assert!(offenders(
            "def f():\n    try:\n        pass\n    except* ValueError:\n        for i in range(3):\n            break\n"
        )
        .is_empty());
        assert!(offenders(
            "def f():\n    try:\n        pass\n    except* ValueError:\n        while 1:\n            continue\n"
        )
        .is_empty());
    }

    #[test]
    fn jump_in_a_loop_else_clause_targets_the_outer_loop_and_is_rejected() {
        // `for ... else:` runs after the loop finishes, so a `break` there
        // binds to the *enclosing* loop — CPython rejects it.
        assert_eq!(
            offenders(
                "def f():\n    for j in range(2):\n        try:\n            pass\n        except* ValueError:\n            for i in range(3):\n                pass\n            else:\n                break\n"
            ),
            vec!["break"]
        );
    }

    #[test]
    fn nested_function_and_class_bodies_are_exempt() {
        assert!(offenders(
            "def f():\n    try:\n        pass\n    except* ValueError:\n        def g():\n            return 1\n"
        )
        .is_empty());
        assert!(offenders(
            "def f():\n    try:\n        pass\n    except* ValueError:\n        class C:\n            def m(self):\n                return 1\n"
        )
        .is_empty());
    }

    #[test]
    fn try_body_else_and_finally_are_not_part_of_the_except_star_block() {
        assert!(offenders(
            "def f():\n    try:\n        return 1\n    except* ValueError:\n        pass\n"
        )
        .is_empty());
        assert!(offenders(
            "def f():\n    try:\n        pass\n    except* ValueError:\n        pass\n    else:\n        return 1\n"
        )
        .is_empty());
        assert!(offenders(
            "def f():\n    try:\n        pass\n    except* ValueError:\n        pass\n    finally:\n        return 1\n"
        )
        .is_empty());
    }

    #[test]
    fn plain_except_is_never_flagged() {
        assert!(offenders(
            "def f():\n    try:\n        pass\n    except ValueError:\n        return 1\n"
        )
        .is_empty());
        assert!(offenders(
            "def f():\n    for j in range(2):\n        try:\n            pass\n        except ValueError:\n            continue\n"
        )
        .is_empty());
    }

    #[test]
    fn except_star_nested_inside_a_plain_handler_is_still_checked() {
        assert_eq!(
            offenders(
                "def f():\n    try:\n        pass\n    except ValueError:\n        try:\n            pass\n        except* TypeError:\n            return 1\n"
            ),
            vec!["return"]
        );
    }

    #[test]
    fn a_return_inside_a_nested_plain_handler_of_an_except_star_is_rejected() {
        // Lexically still inside the `except*` block — CPython rejects it.
        assert_eq!(
            offenders(
                "def f():\n    try:\n        pass\n    except* ValueError:\n        try:\n            pass\n        except TypeError:\n            return 1\n"
            ),
            vec!["return"]
        );
    }

    #[test]
    fn every_offender_is_reported_not_just_the_first() {
        let out = offenders(
            "def f():\n    for j in range(2):\n        try:\n            pass\n        except* ValueError:\n            if j:\n                return 1\n            break\n",
        );
        assert_eq!(out, vec!["return", "break"]);
    }

    #[test]
    fn diagnostic_carries_the_expected_code() {
        let module = tyc_syntax::parse_module(
            "def f():\n    try:\n        pass\n    except* ValueError:\n        return 1\n",
        )
        .expect("parse failed")
        .into_syntax();
        let diags = analyse_except_star_control_flow(&module, "t.py", "");
        let err = &diags.errors()[0];
        let code = miette::Diagnostic::code(err).expect("code").to_string();
        assert_eq!(code, "tyc::return_in_except_star");
    }
}

#[cfg(test)]
mod secret_table_tests {
    use super::*;

    /// The invariant every consumer relies on for most-specific-first
    /// reporting: a keyword that contains another keyword as a substring
    /// must be ordered before it. Violations of exactly this rule shipped
    /// in v1.0.0-alpha.4 (`KEY` before `APIKEY`, so `KEY_APIKEY` reported
    /// the bare `KEY`) and were caught again in the alpha.9 review
    /// (`DB_PWD` initially placed after `PWD`). This test makes the next
    /// violation a compile-gate failure instead of a review catch.
    #[test]
    fn secret_keyword_table_is_longest_first() {
        for (i, a) in SECRET_NAME_KEYWORDS.iter().enumerate() {
            for (j, b) in SECRET_NAME_KEYWORDS.iter().enumerate() {
                if i != j && a.contains(b) {
                    assert!(
                        i < j,
                        "`{a}` contains `{b}` but is ordered after it \
                         (index {i} vs {j}); a name matching both would \
                         report the less-specific `{b}`"
                    );
                }
            }
        }
    }

    #[test]
    fn secret_keyword_table_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for word in SECRET_NAME_KEYWORDS {
            assert!(seen.insert(word), "duplicate keyword `{word}`");
        }
    }

    #[test]
    fn secret_keyword_match_reports_most_specific_word() {
        // Boundary behaviour itself is pinned in depth by the CLI crate's
        // `secret_suffix_*` tests (which now exercise this same shared
        // implementation); keep a couple of canaries here beside the table.
        assert_eq!(secret_keyword_match("KEY_APIKEY"), Some("APIKEY"));
        assert_eq!(secret_keyword_match("DB_PWD"), Some("DB_PWD"));
        assert_eq!(secret_keyword_match("dbPASSWORDstring"), Some("DBPASSWORD"));
        assert_eq!(secret_keyword_match("TOKENs"), Some("TOKEN"));
        assert_eq!(secret_keyword_match("MONKEY"), None);
        assert_eq!(secret_keyword_match("PASSPORT"), None);
    }
}
