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

use std::collections::HashMap;

use ruff_python_ast::{Decorator, Expr, ExprCall, ModModule, Number, Parameters, Stmt};
use tyc_diagnostics::{Diagnostics, TycError};
use tyc_syntax::preprocess::ComptimeBinding;

/// Recursion limit for `comptime def` calls. Generous enough for any
/// realistic build-time configuration computation, low enough that an
/// infinite recursion bug terminates the build instead of hanging it.
const MAX_COMPTIME_DEPTH: usize = 64;

pub mod auto_gather;
pub use auto_gather::{
    collect_gatherable_async_fn_names, detect_missed_gathers, rewrite_auto_gather, AutoGatherStats,
    MissedGather,
};

pub mod pgo;
pub use pgo::{load_profile_samples, pgo_memoise_targets, ProfileSample};

pub mod parallel;
pub use parallel::{rewrite_parallel_comprehensions, ParallelStats};

pub mod extend_builtin;
pub use extend_builtin::{
    extract_builtin_extensions, rewrite_builtin_extension_calls, ExtensionExtractionStats,
    ExtensionRegistry,
};

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
            ComptimeValue::Float(f) => {
                let s = format!("{}", f);
                // Ensure Python can parse it as a float (add `.0` if needed).
                if s.contains('.') || s.contains('e') || s.contains('E') {
                    s
                } else {
                    format!("{}.0", s)
                }
            }
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
                ann.value = Some(Box::new(comptime_value_to_expr(cv)));
                return Stmt::AnnAssign(ann);
            }
        }
        Stmt::AnnAssign(ann)
    } else {
        stmt
    }
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
                diags.push_error(TycError::comptime(
                    binding.name.clone(),
                    format!("comptime binding '{}' has no initialiser", binding.name),
                ));
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
}

impl<'a> EvalContext<'a> {
    fn new(functions: &'a HashMap<&'a str, ComptimeFnDef<'a>>) -> Self {
        Self {
            functions,
            locals: HashMap::new(),
            depth: 0,
        }
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
                (ruff_python_ast::UnaryOp::USub, ComptimeValue::Int(n)) => {
                    Ok(ComptimeValue::Int(-n))
                }
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
            eval_binop(b.op, lhs, rhs)
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
        ComptimeValue::Float(f) => {
            if f.is_finite() && f.fract() == 0.0 {
                Ok(format!("{:.1}", f))
            } else {
                Ok(format!("{}", f))
            }
        }
        ComptimeValue::Str(s) => Ok(s.clone()),
        ComptimeValue::Bool(b) => Ok(if *b { "True" } else { "False" }.to_owned()),
        ComptimeValue::Type(t) => Ok(t.clone()),
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
    // Promote int/float to a common float for ordering when types mix.
    fn as_f64(v: &ComptimeValue) -> Option<f64> {
        match v {
            ComptimeValue::Int(n) => Some(*n as f64),
            ComptimeValue::Float(f) => Some(*f),
            _ => None,
        }
    }
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
        (Lt | LtE | Gt | GtE, a, b) => match (as_f64(a), as_f64(b)) {
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

fn values_equal(a: &ComptimeValue, b: &ComptimeValue) -> bool {
    match (a, b) {
        (ComptimeValue::Int(x), ComptimeValue::Int(y)) => x == y,
        (ComptimeValue::Float(x), ComptimeValue::Float(y)) => x == y,
        (ComptimeValue::Int(x), ComptimeValue::Float(y)) => (*x as f64) == *y,
        (ComptimeValue::Float(x), ComptimeValue::Int(y)) => *x == (*y as f64),
        (ComptimeValue::Str(x), ComptimeValue::Str(y)) => x == y,
        (ComptimeValue::Bool(x), ComptimeValue::Bool(y)) => x == y,
        (ComptimeValue::Type(x), ComptimeValue::Type(y)) => x == y,
        _ => false,
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
                ComptimeValue::Float(f) => Ok(ComptimeValue::Int(f as i64)),
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
                ComptimeValue::Float(f) => f.to_string(),
                ComptimeValue::Bool(b) => if b { "True" } else { "False" }.into(),
                ComptimeValue::Type(t) => t,
                // Reject `str(container)` at comptime: matching Python's
                // `str(["a"])` -> `"['a']"` (single-quoted nested
                // strings) would require a separate Python-flavoured
                // repr serialiser. `to_python_literal` always emits
                // double-quoted strings, which is valid Python source
                // but differs from Python's runtime `str()` output by
                // one character per nested string. Better to reject
                // than silently produce a value that contradicts what
                // the same expression would compute at runtime.
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
            Ok(ComptimeValue::Str(s.replace(needle, replacement)))
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
            .ok_or_else(|| "integer overflow in comptime addition".to_string()),
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
            .ok_or_else(|| "integer overflow in comptime subtraction".to_string()),
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
            .ok_or_else(|| "integer overflow in comptime multiplication".to_string()),
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
        _ => Err(format!(
            "operator is not supported between these comptime value types: {:?} {:?} {:?}",
            op, lhs, rhs
        )),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

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
    /// Empty when the function satisfies every purity condition; otherwise the
    /// first reason it fails.
    pub violation: Option<String>,
    /// Byte span of the `def` name (for diagnostic placement).
    pub span: (usize, usize),
}

/// Walk every top-level function in `module` and report on its purity status.
///
/// `auto_memoise` is the value of `[strictness] auto-memoise` from the project
/// `typhon.toml` (defaulting to `false`). When `true`, every pure function is
/// treated as if the user had written `@memo` so the desugarer emits a cache
/// decorator.
pub fn analyse_purity(module: &ModModule, auto_memoise: bool) -> Vec<PurityFinding> {
    let mut out = Vec::new();
    // Phase 1: collect a module-scope view that purity decisions depend
    // on. We need three things:
    //   - The set of module-level names introduced by *any* assignment
    //     (including `AnnAssign`). Pure functions are forbidden from
    //     mutating these through attribute or subscript writes.
    //   - The set of class names declared at module level — they double
    //     as legitimate constructors and so are pure-callable.
    //   - The set of user-defined functions declared at module level,
    //     keyed by name, along with whether each is itself declared pure
    //     (so we can enforce transitive purity through call graphs).
    let scope = ModuleScope::collect(&module.body, auto_memoise);
    analyse_stmts(
        &module.body,
        &scope,
        auto_memoise,
        &mut out,
        /*async_context=*/ false,
    );
    out
}

/// Snapshot of the module surface that purity decisions need to consult.
#[derive(Debug, Default)]
struct ModuleScope {
    /// Names bound at module level by any assignment form. A pure function
    /// is allowed to *read* these but not to mutate them via attribute or
    /// subscript writes (`MODULE_LIST.append(x)` / `MODULE_DICT[k] = v`).
    module_names: Vec<String>,
    /// Class names declared at module level. Class names are treated as
    /// pure callables (the default `@dataclass(slots=True)` emission has
    /// no side effects).
    class_names: Vec<String>,
    /// User-defined function name → whether the function is itself declared
    /// pure (`@pure` / `@memo` / `@pure(memo=True)` / auto-memoise). When
    /// a `@pure` function calls another module-defined function, the callee
    /// must also be in this map with `true` to satisfy transitive purity.
    user_functions: HashMap<String, bool>,
}

impl ModuleScope {
    fn collect(body: &[Stmt], auto_memoise: bool) -> Self {
        let mut s = Self::default();
        for stmt in body {
            match stmt {
                Stmt::Assign(a) => {
                    for t in &a.targets {
                        if let Expr::Name(n) = t {
                            s.module_names.push(n.id.as_str().to_owned());
                        }
                    }
                }
                Stmt::AnnAssign(a) => {
                    if let Expr::Name(n) = a.target.as_ref() {
                        s.module_names.push(n.id.as_str().to_owned());
                    }
                }
                Stmt::ClassDef(c) => {
                    s.class_names.push(c.name.as_str().to_owned());
                }
                Stmt::FunctionDef(f) => {
                    let (declared, _) = decorator_intent(&f.decorator_list, auto_memoise);
                    s.user_functions
                        .insert(f.name.as_str().to_owned(), declared);
                }
                _ => {}
            }
        }
        s
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
    for stmt in body {
        if let Stmt::FunctionDef(f) = stmt {
            let (declared, memo) = decorator_intent(&f.decorator_list, auto_memoise);
            // Run the purity check whenever the user opted in OR the
            // project asked for automatic caching. Auto-memoise never
            // produces an error (see `purity_diagnostics`); it just
            // gates cache-decorator injection on the function silently
            // passing.
            if declared || memo {
                let violation =
                    check_purity(f.name.as_str(), &f.parameters, &f.body, f.is_async, module);
                out.push(PurityFinding {
                    name: f.name.as_str().to_owned(),
                    declared_pure: declared,
                    memoise: memo,
                    violation,
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
fn decorator_intent(decorators: &[Decorator], auto_memoise: bool) -> (bool, bool) {
    let mut declared = false;
    let mut memoise = auto_memoise;
    for d in decorators {
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

fn check_purity(
    name: &str,
    parameters: &Parameters,
    body: &[Stmt],
    is_async: bool,
    module: &ModuleScope,
) -> Option<String> {
    let _ = name;
    // 1. Synchronous.
    if is_async {
        return Some("function is async — pure functions must be synchronous".into());
    }
    // 2. Hashable parameter types — memoisation backs every cache hit with a
    //    dict keyed on `args`. Reject obviously unhashable annotations so the
    //    cache decorator the desugarer injects never crashes at runtime.
    if let Some(reason) = unhashable_param_reason(parameters) {
        return Some(reason);
    }
    // The remaining four conditions are checked by walking the body.
    let mut ctx = PurityCtx {
        violation: None,
        module,
    };
    walk_stmts_purity(body, &mut ctx);
    ctx.violation
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

struct PurityCtx<'a> {
    violation: Option<String>,
    module: &'a ModuleScope,
}

impl PurityCtx<'_> {
    fn fail(&mut self, reason: impl Into<String>) {
        if self.violation.is_none() {
            self.violation = Some(reason.into());
        }
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
            if let Some(reason) = forbidden_callee(&c.func) {
                ctx.fail(reason);
                return;
            }
            // Transitive purity: a `@pure` function that calls another
            // module-defined function may only do so when the callee is
            // itself declared pure. Builtin callables and module-level class
            // constructors are allowed by the known-pure allow-list.
            if let Expr::Name(n) = c.func.as_ref() {
                let callee = n.id.as_str();
                if let Some(reason) = check_callee_purity(callee, ctx.module) {
                    ctx.fail(reason);
                    return;
                }
            }
            walk_expr_purity(&c.func, ctx);
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
        _ => {}
    }
}

/// Check whether an assignment / aug-assign / delete target mutates state
/// the pure function isn't allowed to touch. Bare-name targets are local
/// variables (Python scoping makes them so once `global` is forbidden) and
/// are fine; attribute and subscript targets whose root is a module-level
/// binding are not.
fn check_mutation_target(target: &Expr, ctx: &mut PurityCtx) {
    match target {
        Expr::Name(_) => {
            // Local. `Stmt::Global` is already a hard error, so any bare name
            // here is guaranteed to be a function-local binding by Python's
            // own scoping rules.
        }
        Expr::Attribute(_) | Expr::Subscript(_) => {
            if let Some(root) = mutation_root_name(target) {
                if ctx.module.module_names.iter().any(|m| m == &root) {
                    ctx.fail(format!(
                        "pure functions must not mutate module-level state \
                         (`{}.…` or `{}[…]` would write to a binding declared at module scope)",
                        root, root
                    ));
                }
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
/// I/O, no clocks, no entropy, no mutation of arguments). Constructors of
/// hashable / immutable types and basic transformations are included; any
/// callable that materialises a mutable container (`list`, `dict`, `set`,
/// `bytearray`) is intentionally excluded because constructing one inside a
/// pure function leaks identity through the cache.
fn is_pure_builtin(name: &str) -> bool {
    matches!(
        name,
        // Transformations / queries on immutable args.
        "abs" | "all" | "any" | "ascii" | "bin" | "bool" | "callable"
        | "chr" | "complex" | "divmod" | "enumerate" | "filter"
        | "float" | "format" | "frozenset" | "hasattr" | "hash" | "hex"
        | "id" | "int" | "isinstance" | "issubclass" | "iter" | "len"
        | "map" | "max" | "min" | "next" | "oct" | "ord" | "pow"
        | "range" | "repr" | "reversed" | "round" | "sorted" | "str"
        | "sum" | "tuple" | "type" | "vars" | "zip" | "bytes" | "object"
        // Typhon `Result` constructors emitted by the desugar pass.
        | "Ok" | "Err" | "Result"
    )
}

/// If `func` references a forbidden callable (I/O, entropy, clock), return a
/// concrete reason string. Otherwise return `None`.
fn forbidden_callee(func: &Expr) -> Option<String> {
    let path = dotted_path(func)?;
    // Bare-name builtins.
    match path.as_str() {
        "print" | "open" | "input" | "exec" | "eval" | "compile" => {
            return Some(format!("pure functions must not call I/O builtin `{path}`"));
        }
        _ => {}
    }
    // Stdlib module attributes.
    if path.starts_with("os.")
        || path.starts_with("sys.stdout")
        || path.starts_with("sys.stderr")
        || path.starts_with("sys.stdin")
        || path.starts_with("subprocess.")
        || path.starts_with("socket.")
        || path.starts_with("requests.")
        || path.starts_with("urllib.")
        || path.starts_with("httpx.")
        || path.starts_with("logging.")
    {
        return Some(format!(
            "pure functions must not perform I/O — call `{path}` is impure"
        ));
    }
    if path.starts_with("time.time")
        || path.starts_with("time.monotonic")
        || path.starts_with("time.perf_counter")
    {
        return Some(format!(
            "pure functions must not read a clock — `{path}` is non-deterministic"
        ));
    }
    if path.starts_with("random.")
        || path.starts_with("secrets.")
        || path.starts_with("uuid.uuid")
        || path == "os.urandom"
    {
        return Some(format!(
            "pure functions must not read entropy — `{path}` is non-deterministic"
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
) -> Diagnostics {
    let mut diags = Diagnostics::new();
    walk_secret_literal_stmts(&module.body, path, source, &mut diags);
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

/// True when `name` ends with one of the recognised secret-shaped
/// suffixes. Match is case-insensitive on the suffix; the suffix may
/// either form the whole name (e.g. `TOKEN`) or follow an underscore
/// (e.g. `MY_TOKEN`). The expected callers feed module-level binding
/// names; passing a function or method name through is harmless because
/// the caller already gated on `Stmt::Assign` / `Stmt::AnnAssign`.
fn is_secret_name(name: &str) -> bool {
    // Recognised suffixes. Longest-first matters because of
    // `API_KEY` overlapping `KEY` — both fire, but the help text
    // remains the same so the order is purely defensive.
    const SUFFIXES: &[&str] = &["API_KEY", "PASSWORD", "TOKEN", "SECRET", "PWD", "KEY"];
    let upper = name.to_ascii_uppercase();
    for suffix in SUFFIXES {
        if upper == *suffix {
            return true;
        }
        if upper.ends_with(suffix) {
            // Ensure the suffix is preceded by `_` so `MONKEY` doesn't
            // match `KEY` and `PASSPORT` doesn't match nothing useful.
            let prefix_len = upper.len() - suffix.len();
            if upper.as_bytes()[prefix_len - 1] == b'_' {
                return true;
            }
        }
    }
    false
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
    use tyc_syntax::preprocess::preprocess;

    fn parse(src: &str) -> ModModule {
        let prep = preprocess(src);
        tyc_syntax::parse_module(&prep.python_source)
            .expect("parse failed")
            .into_syntax()
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
        let diags = analyse_secret_literal_bindings(&module, "x.ty", "API_TOKEN = \"abc\"\n");
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
        let diags = analyse_secret_literal_bindings(&module, "x.ty", src);
        assert_eq!(diags.warnings().len(), 2);
    }

    #[test]
    fn secret_literal_fires_on_openai_api_key() {
        let src = "OPENAI_API_KEY = \"sk-foo\"\n";
        let module = parse(src);
        let diags = analyse_secret_literal_bindings(&module, "x.ty", src);
        assert_eq!(diags.warnings().len(), 1);
    }

    #[test]
    fn secret_literal_fires_on_my_secret() {
        let src = "MY_SECRET = \"abc\"\n";
        let module = parse(src);
        let diags = analyse_secret_literal_bindings(&module, "x.ty", src);
        assert_eq!(diags.warnings().len(), 1);
    }

    #[test]
    fn secret_literal_silent_on_env_lookup() {
        // The whole point of the lint: env-driven RHS should NOT warn.
        let src = "import os\nAPI_TOKEN = os.getenv(\"API_TOKEN\")\n";
        let module = parse(src);
        let diags = analyse_secret_literal_bindings(&module, "x.ty", src);
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
        let diags = analyse_secret_literal_bindings(&module, "x.ty", src);
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
        let diags = analyse_secret_literal_bindings(&module, "x.ty", &prep.python_source);
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
}
