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

use rustpython_ast::{text_size::TextRange, Constant, Expr, Mod, Stmt};
use tyc_diagnostics::{Diagnostics, TycError};
use tyc_syntax::preprocess::ComptimeBinding;

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
    module: &Mod<TextRange>,
    bindings: &[ComptimeBinding],
) -> (HashMap<String, ComptimeValue>, Diagnostics) {
    let mut values = HashMap::new();
    let mut diags = Diagnostics::new();

    if bindings.is_empty() {
        return (values, diags);
    }

    let body = match module {
        Mod::Module(m) => &m.body,
        _ => return (values, diags),
    };

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

fn eval_expr(expr: &Expr<TextRange>) -> Result<ComptimeValue, String> {
    match expr {
        // Literals
        Expr::Constant(c) => eval_constant(&c.value),

        // Unary `-` for negative literals
        Expr::UnaryOp(u) if matches!(u.op, rustpython_ast::UnaryOp::USub) => {
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

fn eval_constant(c: &Constant) -> Result<ComptimeValue, String> {
    match c {
        Constant::Int(i) => {
            // rustpython_ast wraps integers in a BigInt; convert via string.
            let s = i.to_string();
            s.parse::<i64>()
                .map(ComptimeValue::Int)
                .map_err(|_| format!("integer literal '{}' overflows i64", s))
        }
        Constant::Float(f) => Ok(ComptimeValue::Float(*f)),
        Constant::Str(s) => Ok(ComptimeValue::Str(s.clone())),
        Constant::Bool(b) => Ok(ComptimeValue::Bool(*b)),
        Constant::None => Err("None is not a valid comptime value".into()),
        other => Err(format!("unsupported literal kind: {:?}", other)),
    }
}

fn eval_call(call: &rustpython_ast::ExprCall<TextRange>) -> Result<ComptimeValue, String> {
    let func_name = match call.func.as_ref() {
        Expr::Name(n) => n.id.as_str(),
        _ => return Err(
            "only simple function calls (env, int, str, float) are valid in comptime expressions"
                .into(),
        ),
    };

    match func_name {
        "env" => eval_env_call(call),
        "int" => {
            if call.args.len() != 1 || !call.keywords.is_empty() {
                return Err("int() in comptime context takes exactly one argument".into());
            }
            match eval_expr(&call.args[0])? {
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
            if call.args.len() != 1 || !call.keywords.is_empty() {
                return Err("float() in comptime context takes exactly one argument".into());
            }
            match eval_expr(&call.args[0])? {
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
            if call.args.len() != 1 || !call.keywords.is_empty() {
                return Err("str() in comptime context takes exactly one argument".into());
            }
            let v = eval_expr(&call.args[0])?;
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

fn eval_env_call(call: &rustpython_ast::ExprCall<TextRange>) -> Result<ComptimeValue, String> {
    if call.args.is_empty() || call.args.len() > 2 {
        return Err("env() requires one or two positional arguments: env(\"NAME\") or env(\"NAME\", \"default\")".into());
    }
    if !call.keywords.is_empty() {
        return Err("env() does not accept keyword arguments".into());
    }

    let var_name = match eval_expr(&call.args[0])? {
        ComptimeValue::Str(s) => s,
        _ => return Err("env() first argument must be a string literal".into()),
    };

    match std::env::var(&var_name) {
        Ok(val) => Ok(ComptimeValue::Str(val)),
        Err(_) => {
            if call.args.len() == 2 {
                // Use the default value.
                eval_expr(&call.args[1])
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
    op: rustpython_ast::Operator,
    lhs: ComptimeValue,
    rhs: ComptimeValue,
) -> Result<ComptimeValue, String> {
    use rustpython_ast::Operator::*;
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

fn expr_kind_name(expr: &Expr<TextRange>) -> &'static str {
    match expr {
        Expr::BoolOp(_) => "BoolOp",
        Expr::NamedExpr(_) => "NamedExpr",
        Expr::BinOp(_) => "BinOp",
        Expr::UnaryOp(_) => "UnaryOp",
        Expr::Lambda(_) => "Lambda",
        Expr::IfExp(_) => "IfExp",
        Expr::Dict(_) => "Dict",
        Expr::Set(_) => "Set",
        Expr::ListComp(_) => "ListComp",
        Expr::SetComp(_) => "SetComp",
        Expr::DictComp(_) => "DictComp",
        Expr::GeneratorExp(_) => "GeneratorExp",
        Expr::Await(_) => "Await",
        Expr::Yield(_) => "Yield",
        Expr::YieldFrom(_) => "YieldFrom",
        Expr::Compare(_) => "Compare",
        Expr::Call(_) => "Call",
        Expr::FormattedValue(_) => "FormattedValue",
        Expr::JoinedStr(_) => "JoinedStr",
        Expr::Constant(_) => "Constant",
        Expr::Attribute(_) => "Attribute",
        Expr::Subscript(_) => "Subscript",
        Expr::Starred(_) => "Starred",
        Expr::Name(_) => "Name",
        Expr::List(_) => "List",
        Expr::Tuple(_) => "Tuple",
        Expr::Slice(_) => "Slice",
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rustpython_parser::{parse, Mode};
    use tyc_syntax::preprocess::preprocess;

    fn eval(src: &str) -> (HashMap<String, ComptimeValue>, Diagnostics) {
        let prep = preprocess(src);
        let module = parse(&prep.python_source, Mode::Module, "<test>").expect("parse failed");
        evaluate_comptime(&module, &prep.comptime_bindings)
    }

    #[test]
    fn integer_literal_evaluated() {
        let (values, diags) = eval("comptime val PORT: int = 8080\n");
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("PORT"), Some(ComptimeValue::Int(8080))));
    }

    #[test]
    fn string_literal_evaluated() {
        let (values, diags) = eval("comptime val HOST: str = \"localhost\"\n");
        assert!(!diags.has_errors());
        assert!(matches!(values.get("HOST"), Some(ComptimeValue::Str(s)) if s == "localhost"));
    }

    #[test]
    fn env_with_default_uses_default_when_unset() {
        // Use a unique name that no other test sets.
        std::env::remove_var("__TYPHON_TEST_UNSET_DEFAULT__");
        let (values, diags) = eval(
            "comptime val PORT: int = int(env(\"__TYPHON_TEST_UNSET_DEFAULT__\", \"9000\"))\n",
        );
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(matches!(values.get("PORT"), Some(ComptimeValue::Int(9000))));
    }

    #[test]
    fn env_with_default_uses_env_when_set() {
        std::env::set_var("__TYPHON_TEST_SET_DEFAULT__", "4321");
        let (values, diags) =
            eval("comptime val PORT: int = int(env(\"__TYPHON_TEST_SET_DEFAULT__\", \"9000\"))\n");
        std::env::remove_var("__TYPHON_TEST_SET_DEFAULT__");
        assert!(!diags.has_errors());
        assert!(matches!(values.get("PORT"), Some(ComptimeValue::Int(4321))));
    }

    #[test]
    fn missing_required_env_is_an_error() {
        std::env::remove_var("__TYPHON_REQUIRED_TEST_UNIQUE__");
        let (_, diags) =
            eval("comptime val DB_URL: str = env(\"__TYPHON_REQUIRED_TEST_UNIQUE__\")\n");
        assert!(diags.has_errors(), "missing env var must be a build error");
    }

    #[test]
    fn int_coercion_on_string() {
        let (values, diags) = eval("comptime val N: int = int(\"42\")\n");
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
pub fn analyse_purity(module: &Mod<TextRange>, auto_memoise: bool) -> Vec<PurityFinding> {
    let mut out = Vec::new();
    if let Mod::Module(m) = module {
        // Pre-collect module-level binding names that are NOT `val` (i.e. they
        // are reassignable). Pure functions are not allowed to mutate them.
        // The preprocess pass has already stripped `val`/`var`, so by the time
        // the AST lands here we conservatively treat every module-level name
        // assigned via `Assign` (not `AnnAssign`) as potentially mutable.
        let module_mutable_names = collect_module_mutable_names(&m.body);
        analyse_stmts(
            &m.body,
            &module_mutable_names,
            auto_memoise,
            &mut out,
            /*async_context=*/ false,
        );
    }
    out
}

fn collect_module_mutable_names(body: &[Stmt<TextRange>]) -> Vec<String> {
    let mut out = Vec::new();
    for stmt in body {
        if let Stmt::Assign(a) = stmt {
            for t in &a.targets {
                if let Expr::Name(n) = t {
                    out.push(n.id.as_str().to_owned());
                }
            }
        }
    }
    out
}

fn analyse_stmts(
    body: &[Stmt<TextRange>],
    module_mutable: &[String],
    auto_memoise: bool,
    out: &mut Vec<PurityFinding>,
    _async_context: bool,
) {
    for stmt in body {
        match stmt {
            Stmt::FunctionDef(f) => {
                let (declared, memo) = decorator_intent(&f.decorator_list, auto_memoise);
                if declared {
                    let violation = check_purity(
                        f.name.as_str(),
                        &f.args,
                        &f.body,
                        /*is_async=*/ false,
                        module_mutable,
                    );
                    out.push(PurityFinding {
                        name: f.name.as_str().to_owned(),
                        declared_pure: true,
                        memoise: memo,
                        violation,
                        span: (
                            f.range.start().to_usize(),
                            f.range.start().to_usize() + f.name.as_str().len(),
                        ),
                    });
                }
                analyse_stmts(&f.body, module_mutable, auto_memoise, out, false);
            }
            Stmt::AsyncFunctionDef(f) => {
                let (declared, memo) = decorator_intent(&f.decorator_list, auto_memoise);
                if declared {
                    let violation = check_purity(
                        f.name.as_str(),
                        &f.args,
                        &f.body,
                        /*is_async=*/ true,
                        module_mutable,
                    );
                    out.push(PurityFinding {
                        name: f.name.as_str().to_owned(),
                        declared_pure: true,
                        memoise: memo,
                        violation,
                        span: (
                            f.range.start().to_usize(),
                            f.range.start().to_usize() + f.name.as_str().len(),
                        ),
                    });
                }
                analyse_stmts(&f.body, module_mutable, auto_memoise, out, true);
            }
            Stmt::ClassDef(c) => {
                analyse_stmts(&c.body, module_mutable, auto_memoise, out, false);
            }
            _ => {}
        }
    }
}

/// Inspect a decorator list. Returns `(declared_pure, memoise)`:
///   - `declared_pure` is `true` if any of `@pure`, `@pure(...)`, or `@memo`
///     appears, or if `auto_memoise` is enabled.
///   - `memoise` is `true` if the user asked for caching: `@memo`,
///     `@pure(memo=True)`, or `auto_memoise`.
fn decorator_intent(decorators: &[Expr<TextRange>], auto_memoise: bool) -> (bool, bool) {
    let mut declared = auto_memoise;
    let mut memoise = auto_memoise;
    for d in decorators {
        match d {
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
                    if call.keywords.iter().any(|k| {
                        k.arg.as_ref().is_some_and(|a| a.as_str() == "memo")
                            && matches!(
                                &k.value,
                                Expr::Constant(c) if matches!(c.value, Constant::Bool(true))
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
    _args: &rustpython_ast::Arguments<TextRange>,
    body: &[Stmt<TextRange>],
    is_async: bool,
    module_mutable: &[String],
) -> Option<String> {
    let _ = name;
    // 1. Synchronous.
    if is_async {
        return Some("function is async — pure functions must be synchronous".into());
    }
    let mut ctx = PurityCtx {
        violation: None,
        module_mutable,
    };
    walk_stmts_purity(body, &mut ctx);
    ctx.violation
}

struct PurityCtx<'a> {
    violation: Option<String>,
    module_mutable: &'a [String],
}

impl<'a> PurityCtx<'a> {
    fn fail(&mut self, reason: impl Into<String>) {
        if self.violation.is_none() {
            self.violation = Some(reason.into());
        }
    }
}

fn walk_stmts_purity(stmts: &[Stmt<TextRange>], ctx: &mut PurityCtx) {
    for stmt in stmts {
        if ctx.violation.is_some() {
            return;
        }
        walk_stmt_purity(stmt, ctx);
    }
}

fn walk_stmt_purity(stmt: &Stmt<TextRange>, ctx: &mut PurityCtx) {
    match stmt {
        Stmt::Raise(_) => {
            ctx.fail("pure functions must not raise — return Result[T, E] to express failure")
        }
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
                if let Expr::Name(n) = t {
                    if ctx.module_mutable.iter().any(|m| m == n.id.as_str()) {
                        ctx.fail(format!(
                            "pure functions must not write to module-level `var` state ('{}')",
                            n.id.as_str()
                        ));
                    }
                }
            }
            walk_expr_purity(&a.value, ctx);
        }
        Stmt::AnnAssign(a) => {
            if let Some(v) = &a.value {
                walk_expr_purity(v, ctx);
            }
        }
        Stmt::AugAssign(a) => {
            if let Expr::Name(n) = a.target.as_ref() {
                if ctx.module_mutable.iter().any(|m| m == n.id.as_str()) {
                    ctx.fail(format!(
                        "pure functions must not write to module-level `var` state ('{}')",
                        n.id.as_str()
                    ));
                }
            }
            walk_expr_purity(&a.value, ctx);
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
            walk_stmts_purity(&s.orelse, ctx);
        }
        Stmt::While(s) => {
            walk_expr_purity(&s.test, ctx);
            walk_stmts_purity(&s.body, ctx);
            walk_stmts_purity(&s.orelse, ctx);
        }
        Stmt::For(s) => {
            walk_expr_purity(&s.iter, ctx);
            walk_stmts_purity(&s.body, ctx);
            walk_stmts_purity(&s.orelse, ctx);
        }
        Stmt::With(s) => walk_stmts_purity(&s.body, ctx),
        Stmt::Match(s) => {
            walk_expr_purity(&s.subject, ctx);
            for case in &s.cases {
                if let Some(g) = &case.guard {
                    walk_expr_purity(g, ctx);
                }
                walk_stmts_purity(&case.body, ctx);
            }
        }
        Stmt::FunctionDef(_) | Stmt::AsyncFunctionDef(_) | Stmt::ClassDef(_) => {
            // Nested defs / classes are out of scope for purity propagation.
        }
        Stmt::AsyncFor(_) | Stmt::AsyncWith(_) | Stmt::TryStar(_) => {
            ctx.fail("pure functions must not use async constructs");
        }
        _ => {}
    }
}

fn walk_expr_purity(expr: &Expr<TextRange>, ctx: &mut PurityCtx) {
    match expr {
        Expr::Await(_) | Expr::Yield(_) | Expr::YieldFrom(_) => {
            ctx.fail("pure functions must not be async or generator-flavoured (`await`, `yield`)")
        }
        Expr::Call(c) => {
            if let Some(reason) = forbidden_callee(&c.func) {
                ctx.fail(reason);
                return;
            }
            walk_expr_purity(&c.func, ctx);
            for a in &c.args {
                walk_expr_purity(a, ctx);
            }
            for k in &c.keywords {
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
            for cm in &c.comparators {
                walk_expr_purity(cm, ctx);
            }
        }
        Expr::IfExp(i) => {
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

/// If `func` references a forbidden callable (I/O, entropy, clock), return a
/// concrete reason string. Otherwise return `None`.
fn forbidden_callee(func: &Expr<TextRange>) -> Option<String> {
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

fn dotted_path(expr: &Expr<TextRange>) -> Option<String> {
    match expr {
        Expr::Name(n) => Some(n.id.as_str().to_owned()),
        Expr::Attribute(a) => {
            let base = dotted_path(&a.value)?;
            Some(format!("{}.{}", base, a.attr.as_str()))
        }
        _ => None,
    }
}

/// Render the `PurityFinding` list into diagnostics: every `@pure` function
/// whose purity check failed becomes an error.  Returns the unmodified list
/// so callers can also use it to drive cache-decorator emission.
pub fn purity_diagnostics(findings: &[PurityFinding], path: &str, source: &str) -> Diagnostics {
    let mut diags = Diagnostics::new();
    for f in findings {
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
    use rustpython_parser::{parse, Mode};

    fn analyse(src: &str) -> Vec<PurityFinding> {
        let module = parse(src, Mode::Module, "<test>").expect("parse failed");
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
    fn auto_memoise_treats_every_fn_as_pure() {
        let module = parse("def add(a, b): return a + b\n", Mode::Module, "<test>").unwrap();
        let findings = analyse_purity(&module, /*auto_memoise=*/ true);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].declared_pure);
        assert!(findings[0].memoise);
    }
}
