//! Purity, async, comptime, and optimisation analysis (Phase 2+).
//!
//! **Phase 2** implements `comptime` constant evaluation: bindings declared
//! with the `comptime` keyword have their RHS expressions evaluated at build
//! time by [`evaluate_comptime`].  Supported RHS forms:
//!
//! - Integer, float, string, and boolean literals.
//! - `env("NAME")` — reads the environment variable `NAME`; fails the build
//!   if it is unset.
//! - `env("NAME", "default")` — reads `NAME` with a fallback value.
//! - `int(expr)`, `str(expr)`, `float(expr)` — type coercions on the above.
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

pub mod auto_gather;
pub use auto_gather::{collect_gatherable_async_fn_names, rewrite_auto_gather, AutoGatherStats};

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
    let mut values = HashMap::new();
    let mut diags = Diagnostics::new();

    if bindings.is_empty() {
        return (values, diags);
    }

    let body = &module.body;

    for binding in bindings {
        // Find the annotated assignment whose target matches this binding name
        // in the module body.
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
            Some(expr) => match eval_expr(expr) {
                Ok(v) => {
                    values.insert(binding.name.clone(), v);
                }
                Err(e) => {
                    diags.push_error(TycError::comptime(binding.name.clone(), e));
                }
            },
        }
    }

    (values, diags)
}

// ── Expression evaluator ──────────────────────────────────────────────────────

fn eval_expr(expr: &Expr) -> Result<ComptimeValue, String> {
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

        // Unary `-` for negative literals
        Expr::UnaryOp(u) if matches!(u.op, ruff_python_ast::UnaryOp::USub) => {
            match eval_expr(&u.operand)? {
                ComptimeValue::Int(n) => Ok(ComptimeValue::Int(-n)),
                ComptimeValue::Float(f) => Ok(ComptimeValue::Float(-f)),
                _ => Err("unary `-` is only valid on numeric comptime values".into()),
            }
        }

        // Function calls: env(), int(), str(), float()
        Expr::Call(call) => eval_call(call),

        // Binary arithmetic on compile-time numerics and string concatenation.
        Expr::BinOp(b) => {
            let lhs = eval_expr(&b.left)?;
            let rhs = eval_expr(&b.right)?;
            eval_binop(b.op, lhs, rhs)
        }

        other => Err(format!(
            "expression is not a comptime-evaluable constant: {}",
            expr_kind_name(other)
        )),
    }
}

fn eval_call(call: &ExprCall) -> Result<ComptimeValue, String> {
    let func_name = match call.func.as_ref() {
        Expr::Name(n) => n.id.as_str(),
        _ => return Err(
            "only simple function calls (env, int, str, float) are valid in comptime expressions"
                .into(),
        ),
    };

    let args = &call.arguments.args;
    let keywords = &call.arguments.keywords;

    match func_name {
        "env" => eval_env_call(call),
        "int" => {
            if args.len() != 1 || !keywords.is_empty() {
                return Err("int() in comptime context takes exactly one argument".into());
            }
            match eval_expr(&args[0])? {
                ComptimeValue::Str(s) => s
                    .trim()
                    .parse::<i64>()
                    .map(ComptimeValue::Int)
                    .map_err(|_| format!("cannot parse '{}' as int", s)),
                ComptimeValue::Int(n) => Ok(ComptimeValue::Int(n)),
                ComptimeValue::Float(f) => Ok(ComptimeValue::Int(f as i64)),
                ComptimeValue::Bool(b) => Ok(ComptimeValue::Int(if b { 1 } else { 0 })),
            }
        }
        "float" => {
            if args.len() != 1 || !keywords.is_empty() {
                return Err("float() in comptime context takes exactly one argument".into());
            }
            match eval_expr(&args[0])? {
                ComptimeValue::Str(s) => s
                    .trim()
                    .parse::<f64>()
                    .map(ComptimeValue::Float)
                    .map_err(|_| format!("cannot parse '{}' as float", s)),
                ComptimeValue::Int(n) => Ok(ComptimeValue::Float(n as f64)),
                ComptimeValue::Float(f) => Ok(ComptimeValue::Float(f)),
                ComptimeValue::Bool(b) => Ok(ComptimeValue::Float(if b { 1.0 } else { 0.0 })),
            }
        }
        "str" => {
            if args.len() != 1 || !keywords.is_empty() {
                return Err("str() in comptime context takes exactly one argument".into());
            }
            let v = eval_expr(&args[0])?;
            Ok(ComptimeValue::Str(match v {
                ComptimeValue::Str(s) => s,
                ComptimeValue::Int(n) => n.to_string(),
                ComptimeValue::Float(f) => f.to_string(),
                ComptimeValue::Bool(b) => if b { "True" } else { "False" }.into(),
            }))
        }
        other => Err(format!(
            "function '{}' is not valid in a comptime expression; use env(), int(), str(), or float()",
            other
        )),
    }
}

fn eval_env_call(call: &ExprCall) -> Result<ComptimeValue, String> {
    let args = &call.arguments.args;
    let keywords = &call.arguments.keywords;
    if args.is_empty() || args.len() > 2 {
        return Err("env() requires one or two positional arguments: env(\"NAME\") or env(\"NAME\", \"default\")".into());
    }
    if !keywords.is_empty() {
        return Err("env() does not accept keyword arguments".into());
    }

    let var_name = match eval_expr(&args[0])? {
        ComptimeValue::Str(s) => s,
        _ => return Err("env() first argument must be a string literal".into()),
    };

    match std::env::var(&var_name) {
        Ok(val) => Ok(ComptimeValue::Str(val)),
        Err(_) => {
            if args.len() == 2 {
                // Use the default value.
                eval_expr(&args[1])
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tyc_syntax::preprocess::preprocess;

    fn eval(src: &str) -> (HashMap<String, ComptimeValue>, Diagnostics) {
        let prep = preprocess(src);
        let module = tyc_syntax::parse_module(&prep.python_source)
            .expect("parse failed")
            .into_syntax();
        evaluate_comptime(&module, &prep.comptime_bindings)
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
        std::env::set_var("__TYPHON_TEST_SET_DEFAULT__", "4321");
        let (values, diags) =
            eval("comptime let PORT: int = int(env(\"__TYPHON_TEST_SET_DEFAULT__\", \"9000\"))\n");
        std::env::remove_var("__TYPHON_TEST_SET_DEFAULT__");
        assert!(!diags.has_errors());
        assert!(matches!(values.get("PORT"), Some(ComptimeValue::Int(4321))));
    }

    #[test]
    fn missing_required_env_is_an_error() {
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

// ── Tests ─────────────────────────────────────────────────────────────────────

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
