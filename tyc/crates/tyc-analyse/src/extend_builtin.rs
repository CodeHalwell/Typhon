//! Extension methods on Python built-ins.
//!
//! Pipeline:
//!
//! 1. **Preprocess** has already lowered `extend BUILTIN:` blocks to a
//!    sentinel class shape `class __typhon_builtin_ext_BUILTIN(object): …`.
//! 2. [`extract_builtin_extensions`] walks the module body, finds those
//!    sentinel classes, extracts each method to a module-level free
//!    function `__typhon_ext_BUILTIN__METHOD`, removes the class, and
//!    builds a registry (`BUILTIN → method-name → free-fn-name`).
//! 3. [`rewrite_builtin_extension_calls`] uses the registry plus a small
//!    annotation/parameter table to turn `x.METHOD(args)` into
//!    `__typhon_ext_BUILTIN__METHOD(x, args)` whenever the receiver `x`
//!    has a static annotation of one of the registered built-ins.
//!
//! The rewrite is *strictly opt-in by type annotation*. When the receiver
//! cannot be proven to be the matching built-in, the call is left as a
//! native attribute access — which raises `AttributeError` at runtime,
//! matching Python's existing semantics for missing methods. The
//! conservative bias keeps the rewrite from corrupting legitimate
//! attribute accesses on user-defined types that happen to share a
//! method name with a registered extension.

use std::collections::{HashMap, HashSet};

use ruff_python_ast::{
    name::Name, AtomicNodeIndex, Expr, ExprCall, ExprName, ModModule, Stmt, StmtFunctionDef,
};
use ruff_text_size::TextRange;

/// Marker prefix preprocess uses when lowering `extend BUILTIN:` for one
/// of the recognised Python built-in types.
const STUB_PREFIX: &str = "__typhon_builtin_ext_";

/// Annotate the lifted free function's `self` parameter with the
/// extended builtin's type (FINDINGS #54). Only fires when the first
/// positional parameter is currently unannotated and named `self`; an
/// explicit annotation the user already wrote wins. The annotation
/// node is synthesised with a zero-length `TextRange` so source-map
/// emission inherits the surrounding offset (matching how other
/// desugar passes synthesise AST nodes).
fn annotate_self_param_with_builtin(f: &mut ruff_python_ast::StmtFunctionDef, builtin: &str) {
    let target = f
        .parameters
        .posonlyargs
        .first_mut()
        .or_else(|| f.parameters.args.first_mut());
    let Some(target) = target else { return };
    if target.parameter.name.as_str() != "self" {
        return;
    }
    if target.parameter.annotation.is_some() {
        return;
    }
    target.parameter.annotation = Some(Box::new(Expr::Name(ExprName {
        range: ruff_text_size::TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        id: Name::new(builtin),
        ctx: ruff_python_ast::ExprContext::Load,
    })));
}

/// Free-function naming convention: `__typhon_ext_<TYPE>__<METHOD>__`.
///
/// The trailing `__` matters: CPython name-mangles any identifier that
/// starts with two underscores and does *not* end with two, wherever it
/// appears inside a class body — so a call rewritten inside `impl Post:`
/// (`self.title.slug()` → `__typhon_ext_str__slug__(self.title)`) became
/// `_Post__typhon_ext_str__slug` and raised `NameError`. A dunder-shaped
/// name is exempt from mangling.
fn free_fn_name(ty: &str, method: &str) -> String {
    format!("__typhon_ext_{ty}__{method}__")
}

/// Maps `type-name → method-name → free-function-name`.
pub type ExtensionRegistry = HashMap<String, HashMap<String, String>>;

/// Summary of an extraction pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtensionExtractionStats {
    /// Number of `extend BUILTIN:` blocks consumed.
    pub blocks: usize,
    /// Total number of methods promoted to free functions.
    pub methods: usize,
}

/// Walk `module` and replace every `class __typhon_builtin_ext_BUILTIN(object):`
/// stub with the equivalent set of module-level free-function definitions.
///
/// Returns the extracted registry plus a small statistics struct for
/// diagnostics.  Subsequent passes use the registry to rewrite call sites.
pub fn extract_builtin_extensions(
    module: &mut ModModule,
) -> (ExtensionRegistry, ExtensionExtractionStats) {
    let mut registry: ExtensionRegistry = HashMap::new();
    let mut stats = ExtensionExtractionStats::default();

    let original = std::mem::take(&mut module.body);
    let mut rebuilt: Vec<Stmt> = Vec::with_capacity(original.len());
    for stmt in original {
        if let Stmt::ClassDef(c) = &stmt {
            if let Some(builtin) = c.name.as_str().strip_prefix(STUB_PREFIX) {
                let builtin = builtin.to_owned();
                stats.blocks += 1;
                let entry = registry.entry(builtin.clone()).or_default();
                for member in &c.body {
                    if let Stmt::FunctionDef(f) = member {
                        let mut promoted = f.clone();
                        let new_name = free_fn_name(&builtin, f.name.as_str());
                        promoted.name = ruff_python_ast::Identifier {
                            range: f.name.range,
                            node_index: AtomicNodeIndex::NONE,
                            id: Name::new(&new_name),
                        };
                        // Annotate the receiver (`self`) with the
                        // builtin's type so the lifted free function
                        // satisfies Rule 1 (FINDINGS #54) and `tyc ty`
                        // / pyright / mypy can type-check the body.
                        // The annotation is set only on the first
                        // positional-or-keyword parameter if it is
                        // currently unannotated and named `self`; any
                        // explicit annotation the user already wrote
                        // wins.
                        annotate_self_param_with_builtin(&mut promoted, &builtin);
                        entry.insert(f.name.as_str().to_owned(), new_name);
                        rebuilt.push(Stmt::FunctionDef(promoted));
                        stats.methods += 1;
                    }
                    // Non-function members (docstrings, class-level
                    // assignments) are silently dropped — `extend
                    // BUILTIN:` is for methods only, mirroring the
                    // user-class `impl`-merge contract.
                }
                continue;
            }
        }
        rebuilt.push(stmt);
    }
    module.body = rebuilt;
    (registry, stats)
}

/// Rewrite `x.method(args)` calls into `__typhon_ext_TYPE__method(x, args)`
/// for every receiver whose annotation places it in one of the registered
/// built-in types.
///
/// Returns the number of call sites successfully rewritten.
pub fn rewrite_builtin_extension_calls(
    module: &mut ModModule,
    registry: &ExtensionRegistry,
) -> usize {
    rewrite_builtin_extension_calls_tracking(module, registry).0
}

/// Like [`rewrite_builtin_extension_calls`] but also returns the set of
/// free-function names that were actually used in rewrites. The caller can
/// use this to inject cross-module imports for extension functions defined
/// in another module. (#202)
pub fn rewrite_builtin_extension_calls_tracking(
    module: &mut ModModule,
    registry: &ExtensionRegistry,
) -> (usize, HashSet<String>) {
    if registry.is_empty() {
        return (0, HashSet::new());
    }
    let ctx = RewriteCtx::new(registry, module);
    let mut module_env = collect_module_annotations(module);
    let mut rewrites = 0usize;
    let mut used_fns: HashSet<String> = HashSet::new();
    let body = std::mem::take(&mut module.body);
    let mut new_body: Vec<Stmt> = Vec::with_capacity(body.len());
    for stmt in body {
        new_body.push(rewrite_stmt(
            stmt,
            &ctx,
            &mut module_env,
            &mut rewrites,
            &mut used_fns,
        ));
    }
    module.body = new_body;
    (rewrites, used_fns)
}

/// What one rewrite pass knows beyond the local annotation environment:
/// the extension registry and, for every module class, the built-in type
/// of each annotated field (so `self.title.slug()` inside `impl Post` and
/// `post.title.slug()` on a `post: Post` local both resolve to `str`).
pub(crate) struct RewriteCtx<'a> {
    registry: &'a ExtensionRegistry,
    class_fields: HashMap<String, HashMap<String, String>>,
}

impl<'a> RewriteCtx<'a> {
    fn new(registry: &'a ExtensionRegistry, module: &ModModule) -> Self {
        Self {
            registry,
            class_fields: collect_class_field_types(module),
        }
    }
}

/// Built-in type of every annotated field of every class in `module`
/// (`impl Post:` pseudo-classes are folded into their target class).
fn collect_class_field_types(module: &ModModule) -> HashMap<String, HashMap<String, String>> {
    let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
    for stmt in &module.body {
        if let Stmt::ClassDef(c) = stmt {
            let name = c
                .name
                .as_str()
                .strip_prefix("__typhon_impl_")
                .unwrap_or(c.name.as_str())
                .to_owned();
            let entry = out.entry(name).or_default();
            for member in &c.body {
                if let Stmt::AnnAssign(a) = member {
                    if let (Expr::Name(n), Some(ty)) =
                        (a.target.as_ref(), annotation_to_type(&a.annotation))
                    {
                        entry.insert(n.id.as_str().to_owned(), ty);
                    }
                }
            }
        }
    }
    out
}

/// The `self` binding's class, recorded in the local environment under a
/// key no Python identifier can collide with.
const SELF_CLASS_KEY: &str = "<self-class>";

/// The built-in type a receiver expression evaluates to, when it is
/// evident: an annotated name, a literal or display, a field of an
/// annotated object (`self.title`, `post.title`), or a chain of
/// type-preserving builtin methods (`t.strip().lower()`).
fn receiver_type(expr: &Expr, env: &Env, ctx: &RewriteCtx<'_>) -> Option<String> {
    match expr {
        Expr::Name(n) => env.get(n.id.as_str()).cloned(),
        Expr::StringLiteral(_) | Expr::FString(_) => Some("str".to_owned()),
        Expr::BytesLiteral(_) => Some("bytes".to_owned()),
        Expr::NumberLiteral(n) => Some(
            match n.value {
                ruff_python_ast::Number::Int(_) => "int",
                ruff_python_ast::Number::Float(_) => "float",
                ruff_python_ast::Number::Complex { .. } => "complex",
            }
            .to_owned(),
        ),
        Expr::List(_) | Expr::ListComp(_) => Some("list".to_owned()),
        Expr::Dict(_) | Expr::DictComp(_) => Some("dict".to_owned()),
        Expr::Set(_) | Expr::SetComp(_) => Some("set".to_owned()),
        Expr::Tuple(_) => Some("tuple".to_owned()),
        Expr::Attribute(a) => {
            let owner = match a.value.as_ref() {
                Expr::Name(n) if n.id.as_str() == "self" => env.get(SELF_CLASS_KEY).cloned(),
                other => receiver_type(other, env, ctx),
            }?;
            ctx.class_fields
                .get(&owner)
                .and_then(|fields| fields.get(a.attr.as_str()))
                .cloned()
        }
        Expr::Call(call) => match call.func.as_ref() {
            Expr::Name(n) => match n.id.as_str() {
                "str" | "repr" | "format" | "chr" | "ascii" => Some("str".to_owned()),
                "bytes" => Some("bytes".to_owned()),
                "int" | "len" | "ord" => Some("int".to_owned()),
                "float" => Some("float".to_owned()),
                "list" | "sorted" => Some("list".to_owned()),
                "dict" => Some("dict".to_owned()),
                "set" => Some("set".to_owned()),
                "tuple" => Some("tuple".to_owned()),
                _ => None,
            },
            Expr::Attribute(a) => {
                let recv = receiver_type(&a.value, env, ctx)?;
                let method = a.attr.as_str();
                let preserved = match recv.as_str() {
                    "str" => matches!(
                        method,
                        "strip"
                            | "lstrip"
                            | "rstrip"
                            | "lower"
                            | "upper"
                            | "title"
                            | "capitalize"
                            | "casefold"
                            | "swapcase"
                            | "replace"
                            | "join"
                            | "format"
                            | "format_map"
                            | "center"
                            | "ljust"
                            | "rjust"
                            | "zfill"
                            | "removeprefix"
                            | "removesuffix"
                            | "expandtabs"
                            | "translate"
                    ),
                    "bytes" => matches!(
                        method,
                        "strip" | "lstrip" | "rstrip" | "lower" | "upper" | "replace" | "join"
                    ),
                    "list" => matches!(method, "copy"),
                    "dict" => matches!(method, "copy"),
                    "set" => matches!(
                        method,
                        "copy" | "union" | "intersection" | "difference" | "symmetric_difference"
                    ),
                    _ => false,
                };
                preserved.then_some(recv)
            }
            _ => None,
        },
        _ => None,
    }
}

///
/// Each scope owns its own [`Env`].  Function scopes start *empty* (so a
/// shadowing local rebinding never inherits an outer annotation that
/// happens to share its name); parameters and local `let x: T = ...`
/// statements populate the function scope as the walker descends.
type Env = HashMap<String, String>;

fn collect_module_annotations(module: &ModModule) -> Env {
    let mut env = Env::new();
    for stmt in &module.body {
        if let Stmt::AnnAssign(a) = stmt {
            if let (Expr::Name(n), Some(ty)) =
                (a.target.as_ref(), annotation_to_type(&a.annotation))
            {
                env.insert(n.id.as_str().to_owned(), ty);
            }
        }
    }
    env
}

/// Extract a bare built-in type name from an annotation expression.
/// Supports `str`, `int`, `list`, `dict`, … plus the bracketed generic
/// forms `list[int]` and `dict[str, int]` (which still anchor on the
/// outer built-in name).
fn annotation_to_type(ann: &Expr) -> Option<String> {
    match ann {
        Expr::Name(n) => Some(n.id.as_str().to_owned()),
        Expr::Subscript(s) => annotation_to_type(&s.value),
        _ => None,
    }
}

/// Walk one statement, rewriting eligible attribute calls in any
/// expression position and recursing into every nested statement-bearing
/// node (`for`, `while`, `with`, `try`, `match`, …).
///
/// `env` is the current scope's annotation map.  This pass mutates it as
/// it sees `let x: T = ...` so a subsequent statement in the same block
/// can rely on the binding.  Function and class bodies open a *fresh*
/// scope: outer annotations are intentionally dropped at the boundary so
/// a shadowing local name with a different runtime type can never trigger
/// a false-positive rewrite (this is the rule that lets us treat
/// rebinding as opaque).
fn rewrite_stmt(
    stmt: Stmt,
    ctx: &RewriteCtx<'_>,
    env: &mut Env,
    rewrites: &mut usize,
    used_fns: &mut HashSet<String>,
) -> Stmt {
    match stmt {
        Stmt::FunctionDef(mut f) => {
            // Fresh scope: nothing from the enclosing block leaks in —
            // except the enclosing class of `self`, for methods.
            let mut local = Env::new();
            if let Some(cls) = env.get(SELF_CLASS_KEY) {
                local.insert(SELF_CLASS_KEY.to_owned(), cls.clone());
            }
            for param in f.parameters.posonlyargs.iter().chain(
                f.parameters
                    .args
                    .iter()
                    .chain(f.parameters.kwonlyargs.iter()),
            ) {
                if let Some(ann) = &param.parameter.annotation {
                    if let Some(ty) = annotation_to_type(ann) {
                        local.insert(param.parameter.name.as_str().to_owned(), ty);
                    }
                }
            }
            if let Some(vararg) = &f.parameters.vararg {
                if let Some(ann) = &vararg.annotation {
                    if let Some(ty) = annotation_to_type(ann) {
                        local.insert(vararg.name.as_str().to_owned(), ty);
                    }
                }
            }
            if let Some(kwarg) = &f.parameters.kwarg {
                if let Some(ann) = &kwarg.annotation {
                    if let Some(ty) = annotation_to_type(ann) {
                        local.insert(kwarg.name.as_str().to_owned(), ty);
                    }
                }
            }
            f.body = walk_body(
                std::mem::take(&mut f.body),
                ctx,
                &mut local,
                rewrites,
                used_fns,
            );
            Stmt::FunctionDef(f)
        }
        Stmt::ClassDef(mut c) => {
            // Class bodies also open a fresh scope. Methods inherit one
            // fact from it: which class `self` belongs to (an `impl Post:`
            // pseudo-class counts as `Post`), so `self.<field>` receivers
            // can be typed from the class's field annotations.
            let class_name = c
                .name
                .as_str()
                .strip_prefix("__typhon_impl_")
                .unwrap_or(c.name.as_str())
                .to_owned();
            let mut local = Env::new();
            local.insert(SELF_CLASS_KEY.to_owned(), class_name);
            c.body = walk_body(
                std::mem::take(&mut c.body),
                ctx,
                &mut local,
                rewrites,
                used_fns,
            );
            Stmt::ClassDef(c)
        }
        Stmt::AnnAssign(mut a) => {
            if let Some(v) = a.value.as_mut() {
                rewrite_expr(v, ctx, env, rewrites, used_fns);
            }
            // Update the live environment so subsequent statements in
            // this block can rely on the new annotated binding.
            if let (Expr::Name(n), Some(ty)) =
                (a.target.as_ref(), annotation_to_type(&a.annotation))
            {
                env.insert(n.id.as_str().to_owned(), ty);
            }
            Stmt::AnnAssign(a)
        }
        Stmt::Assign(mut a) => {
            rewrite_expr(&mut a.value, ctx, env, rewrites, used_fns);
            // A bare assignment without annotation shadows any prior
            // typed binding for the same name — invalidate it so a
            // later use isn't rewritten against a stale type.
            for target in &a.targets {
                if let Expr::Name(n) = target {
                    env.remove(n.id.as_str());
                }
            }
            Stmt::Assign(a)
        }
        Stmt::AugAssign(mut a) => {
            rewrite_expr(&mut a.value, ctx, env, rewrites, used_fns);
            Stmt::AugAssign(a)
        }
        Stmt::Expr(mut e) => {
            rewrite_expr(&mut e.value, ctx, env, rewrites, used_fns);
            Stmt::Expr(e)
        }
        Stmt::Return(mut r) => {
            if let Some(v) = r.value.as_mut() {
                rewrite_expr(v, ctx, env, rewrites, used_fns);
            }
            Stmt::Return(r)
        }
        Stmt::Raise(mut r) => {
            if let Some(v) = r.exc.as_mut() {
                rewrite_expr(v, ctx, env, rewrites, used_fns);
            }
            if let Some(v) = r.cause.as_mut() {
                rewrite_expr(v, ctx, env, rewrites, used_fns);
            }
            Stmt::Raise(r)
        }
        Stmt::Delete(mut d) => {
            for t in &mut d.targets {
                rewrite_expr(t, ctx, env, rewrites, used_fns);
            }
            Stmt::Delete(d)
        }
        Stmt::If(mut i) => {
            rewrite_expr(&mut i.test, ctx, env, rewrites, used_fns);
            i.body = walk_body(std::mem::take(&mut i.body), ctx, env, rewrites, used_fns);
            for clause in &mut i.elif_else_clauses {
                if let Some(t) = clause.test.as_mut() {
                    rewrite_expr(t, ctx, env, rewrites, used_fns);
                }
                clause.body = walk_body(
                    std::mem::take(&mut clause.body),
                    ctx,
                    env,
                    rewrites,
                    used_fns,
                );
            }
            Stmt::If(i)
        }
        Stmt::While(mut w) => {
            rewrite_expr(&mut w.test, ctx, env, rewrites, used_fns);
            w.body = walk_body(std::mem::take(&mut w.body), ctx, env, rewrites, used_fns);
            w.orelse = walk_body(std::mem::take(&mut w.orelse), ctx, env, rewrites, used_fns);
            Stmt::While(w)
        }
        Stmt::For(mut f) => {
            rewrite_expr(&mut f.target, ctx, env, rewrites, used_fns);
            rewrite_expr(&mut f.iter, ctx, env, rewrites, used_fns);
            f.body = walk_body(std::mem::take(&mut f.body), ctx, env, rewrites, used_fns);
            f.orelse = walk_body(std::mem::take(&mut f.orelse), ctx, env, rewrites, used_fns);
            Stmt::For(f)
        }
        Stmt::With(mut w) => {
            for item in &mut w.items {
                rewrite_expr(&mut item.context_expr, ctx, env, rewrites, used_fns);
                if let Some(v) = item.optional_vars.as_mut() {
                    rewrite_expr(v, ctx, env, rewrites, used_fns);
                }
            }
            w.body = walk_body(std::mem::take(&mut w.body), ctx, env, rewrites, used_fns);
            Stmt::With(w)
        }
        Stmt::Try(mut t) => {
            t.body = walk_body(std::mem::take(&mut t.body), ctx, env, rewrites, used_fns);
            for handler in &mut t.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(h) = handler;
                if let Some(ty) = h.type_.as_mut() {
                    rewrite_expr(ty, ctx, env, rewrites, used_fns);
                }
                h.body = walk_body(std::mem::take(&mut h.body), ctx, env, rewrites, used_fns);
            }
            t.orelse = walk_body(std::mem::take(&mut t.orelse), ctx, env, rewrites, used_fns);
            t.finalbody = walk_body(
                std::mem::take(&mut t.finalbody),
                ctx,
                env,
                rewrites,
                used_fns,
            );
            Stmt::Try(t)
        }
        Stmt::Match(mut m) => {
            rewrite_expr(&mut m.subject, ctx, env, rewrites, used_fns);
            for case in &mut m.cases {
                if let Some(g) = case.guard.as_mut() {
                    rewrite_expr(g, ctx, env, rewrites, used_fns);
                }
                case.body = walk_body(std::mem::take(&mut case.body), ctx, env, rewrites, used_fns);
            }
            Stmt::Match(m)
        }
        other => other,
    }
}

fn walk_body(
    body: Vec<Stmt>,
    ctx: &RewriteCtx<'_>,
    env: &mut Env,
    rewrites: &mut usize,
    used_fns: &mut HashSet<String>,
) -> Vec<Stmt> {
    body.into_iter()
        .map(|s| rewrite_stmt(s, ctx, env, rewrites, used_fns))
        .collect()
}

/// Rewrite eligible attribute calls anywhere inside `expr`.  Descends
/// through every expression variant so a call buried in `x.shout() + "!"`
/// or `f(x.shout())` reaches the rewrite logic.
fn rewrite_expr(
    expr: &mut Expr,
    ctx: &RewriteCtx<'_>,
    env: &Env,
    rewrites: &mut usize,
    used_fns: &mut HashSet<String>,
) {
    // 1. Try the local rewrite first when the head is a callable attribute
    //    on a typed receiver. Other variants fall through to (2) below.
    if let Expr::Call(call) = expr {
        // Recurse into call arguments and (for non-extension calls) the
        // callee expression itself so nested receivers still get fixed.
        for arg in &mut call.arguments.args {
            rewrite_expr(arg, ctx, env, rewrites, used_fns);
        }
        for kw in &mut call.arguments.keywords {
            rewrite_expr(&mut kw.value, ctx, env, rewrites, used_fns);
        }
        if let Expr::Attribute(attr) = call.func.as_ref() {
            if let Some(ty) = receiver_type(&attr.value, env, ctx) {
                {
                    if let Some(methods) = ctx.registry.get(&ty) {
                        if let Some(fn_name) = methods.get(attr.attr.as_str()) {
                            let range = call.range;
                            let receiver = (*attr.value).clone();
                            let mut new_args: Vec<Expr> =
                                Vec::with_capacity(call.arguments.args.len() + 1);
                            new_args.push(receiver);
                            for a in std::mem::take(&mut call.arguments.args).into_vec() {
                                new_args.push(a);
                            }
                            let new_call = ExprCall {
                                range,
                                node_index: AtomicNodeIndex::NONE,
                                func: Box::new(Expr::Name(ExprName {
                                    range,
                                    node_index: AtomicNodeIndex::NONE,
                                    id: Name::new(fn_name),
                                    ctx: ruff_python_ast::ExprContext::Load,
                                })),
                                arguments: ruff_python_ast::Arguments {
                                    range,
                                    node_index: AtomicNodeIndex::NONE,
                                    args: new_args.into_boxed_slice(),
                                    keywords: std::mem::take(&mut call.arguments.keywords),
                                },
                            };
                            *expr = Expr::Call(new_call);
                            *rewrites += 1;
                            used_fns.insert(fn_name.clone());
                            return;
                        }
                    }
                }
            }
        }
        // Not an extension call — still descend into the callee so a
        // receiver buried inside a more complex call expression
        // (lambda, subscript, attribute chain) gets visited.
        rewrite_expr(&mut call.func, ctx, env, rewrites, used_fns);
        return;
    }

    // 2. Generic recursion through every Expr variant. The goal is
    //    coverage, not pretty matching — every shape that can contain
    //    a sub-expression descends.
    match expr {
        Expr::BoolOp(b) => {
            for v in &mut b.values {
                rewrite_expr(v, ctx, env, rewrites, used_fns);
            }
        }
        Expr::Named(n) => {
            rewrite_expr(&mut n.target, ctx, env, rewrites, used_fns);
            rewrite_expr(&mut n.value, ctx, env, rewrites, used_fns);
        }
        Expr::BinOp(b) => {
            rewrite_expr(&mut b.left, ctx, env, rewrites, used_fns);
            rewrite_expr(&mut b.right, ctx, env, rewrites, used_fns);
        }
        Expr::UnaryOp(u) => rewrite_expr(&mut u.operand, ctx, env, rewrites, used_fns),
        Expr::Lambda(l) => {
            // Lambda opens a fresh scope; its parameters can add to a
            // local env. Simpler approach: walk the body with an empty
            // env override so the outer scope doesn't leak in (matches
            // the function-scope rule above).
            let mut local = Env::new();
            if let Some(params) = l.parameters.as_deref() {
                for param in params
                    .posonlyargs
                    .iter()
                    .chain(params.args.iter())
                    .chain(params.kwonlyargs.iter())
                {
                    if let Some(ann) = &param.parameter.annotation {
                        if let Some(ty) = annotation_to_type(ann) {
                            local.insert(param.parameter.name.as_str().to_owned(), ty);
                        }
                    }
                }
            }
            rewrite_expr(&mut l.body, ctx, &local, rewrites, used_fns);
        }
        Expr::If(i) => {
            rewrite_expr(&mut i.test, ctx, env, rewrites, used_fns);
            rewrite_expr(&mut i.body, ctx, env, rewrites, used_fns);
            rewrite_expr(&mut i.orelse, ctx, env, rewrites, used_fns);
        }
        Expr::Dict(d) => {
            for item in &mut d.items {
                if let Some(k) = item.key.as_mut() {
                    rewrite_expr(k, ctx, env, rewrites, used_fns);
                }
                rewrite_expr(&mut item.value, ctx, env, rewrites, used_fns);
            }
        }
        Expr::Set(s) => {
            for e in &mut s.elts {
                rewrite_expr(e, ctx, env, rewrites, used_fns);
            }
        }
        Expr::ListComp(lc) => {
            rewrite_expr(&mut lc.elt, ctx, env, rewrites, used_fns);
            for gen in &mut lc.generators {
                rewrite_expr(&mut gen.iter, ctx, env, rewrites, used_fns);
                for f in &mut gen.ifs {
                    rewrite_expr(f, ctx, env, rewrites, used_fns);
                }
            }
        }
        Expr::SetComp(c) => {
            rewrite_expr(&mut c.elt, ctx, env, rewrites, used_fns);
            for gen in &mut c.generators {
                rewrite_expr(&mut gen.iter, ctx, env, rewrites, used_fns);
                for f in &mut gen.ifs {
                    rewrite_expr(f, ctx, env, rewrites, used_fns);
                }
            }
        }
        Expr::DictComp(c) => {
            if let Some(k) = c.key.as_mut() {
                rewrite_expr(k, ctx, env, rewrites, used_fns);
            }
            rewrite_expr(&mut c.value, ctx, env, rewrites, used_fns);
            for gen in &mut c.generators {
                rewrite_expr(&mut gen.iter, ctx, env, rewrites, used_fns);
                for f in &mut gen.ifs {
                    rewrite_expr(f, ctx, env, rewrites, used_fns);
                }
            }
        }
        Expr::Generator(g) => {
            rewrite_expr(&mut g.elt, ctx, env, rewrites, used_fns);
            for gen in &mut g.generators {
                rewrite_expr(&mut gen.iter, ctx, env, rewrites, used_fns);
                for f in &mut gen.ifs {
                    rewrite_expr(f, ctx, env, rewrites, used_fns);
                }
            }
        }
        Expr::Await(a) => rewrite_expr(&mut a.value, ctx, env, rewrites, used_fns),
        Expr::Yield(y) => {
            if let Some(v) = y.value.as_mut() {
                rewrite_expr(v, ctx, env, rewrites, used_fns);
            }
        }
        Expr::YieldFrom(y) => rewrite_expr(&mut y.value, ctx, env, rewrites, used_fns),
        Expr::Compare(c) => {
            rewrite_expr(&mut c.left, ctx, env, rewrites, used_fns);
            for cmp in &mut c.comparators {
                rewrite_expr(cmp, ctx, env, rewrites, used_fns);
            }
        }
        Expr::Attribute(a) => rewrite_expr(&mut a.value, ctx, env, rewrites, used_fns),
        Expr::Subscript(s) => {
            rewrite_expr(&mut s.value, ctx, env, rewrites, used_fns);
            rewrite_expr(&mut s.slice, ctx, env, rewrites, used_fns);
        }
        Expr::Starred(s) => rewrite_expr(&mut s.value, ctx, env, rewrites, used_fns),
        Expr::List(l) => {
            for e in &mut l.elts {
                rewrite_expr(e, ctx, env, rewrites, used_fns);
            }
        }
        Expr::Tuple(t) => {
            for e in &mut t.elts {
                rewrite_expr(e, ctx, env, rewrites, used_fns);
            }
        }
        Expr::Slice(s) => {
            if let Some(v) = s.lower.as_mut() {
                rewrite_expr(v, ctx, env, rewrites, used_fns);
            }
            if let Some(v) = s.upper.as_mut() {
                rewrite_expr(v, ctx, env, rewrites, used_fns);
            }
            if let Some(v) = s.step.as_mut() {
                rewrite_expr(v, ctx, env, rewrites, used_fns);
            }
        }
        // Leaves and the previously-handled `Call` are no-ops here.
        _ => {}
    }
}

/// Avoid “unused” lint when the field is only structurally referenced.
#[allow(dead_code)]
fn _link_function_def(_: &StmtFunctionDef) -> TextRange {
    TextRange::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tyc_syntax::preprocess::preprocess;

    fn prep_parse(src: &str) -> ModModule {
        let prep = preprocess(src);
        tyc_syntax::parse_module(&prep.python_source)
            .unwrap()
            .into_syntax()
    }

    #[test]
    fn extract_promotes_str_extension_to_free_function() {
        let src = "extend str:\n    def shout(self) -> str:\n        return self.upper()\n";
        let mut m = prep_parse(src);
        let (registry, stats) = extract_builtin_extensions(&mut m);
        assert_eq!(stats.blocks, 1);
        assert_eq!(stats.methods, 1);
        assert!(registry.contains_key("str"));
        assert_eq!(
            registry["str"].get("shout").map(String::as_str),
            Some("__typhon_ext_str__shout__")
        );
        // The stub class is gone from the AST.
        for stmt in &m.body {
            if let Stmt::ClassDef(c) = stmt {
                assert!(
                    !c.name.as_str().starts_with(STUB_PREFIX),
                    "stub class must be removed; saw {}",
                    c.name.as_str()
                );
            }
        }
        // The free function is emitted.
        assert!(m.body.iter().any(|s| matches!(
            s, Stmt::FunctionDef(f) if f.name.as_str() == "__typhon_ext_str__shout__"
        )));
    }

    #[test]
    fn rewrite_call_when_receiver_has_str_annotation() {
        let src = "extend str:\n    def shout(self) -> str:\n        return self.upper()\n\n\
                   greeting: str = \"hi\"\nprint(greeting.shout())\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(rewrites, 1);
        let out = tyc_emit::emit_python(&m);
        assert!(
            out.contains("__typhon_ext_str__shout__(greeting)"),
            "expected call-site rewrite; got:\n{out}"
        );
    }

    #[test]
    fn rewrite_types_field_literal_and_chain_receivers() {
        let src =
            "extend str:\n    def slug(self) -> str:\n        return self.strip().lower()\n\n\
class Post:\n    title: str\n\n\
impl Post:\n    def key(self) -> str:\n        return self.title.slug()\n\n\
def f(t: str, p: Post) -> str:\n    return t.strip().slug() + \"Lit X\".slug() + p.title.slug()\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(rewrites, 4);
        let out = tyc_emit::emit_python(&m);
        assert!(
            out.contains("__typhon_ext_str__slug__(self.title)"),
            "got:\n{out}"
        );
        assert!(
            out.contains("__typhon_ext_str__slug__(t.strip())"),
            "got:\n{out}"
        );
        assert!(
            out.contains("__typhon_ext_str__slug__(\"Lit X\")"),
            "got:\n{out}"
        );
        assert!(
            out.contains("__typhon_ext_str__slug__(p.title)"),
            "got:\n{out}"
        );
    }

    #[test]
    fn rewrite_skips_unannotated_receiver() {
        // No annotation → fallback to native attribute access, which
        // raises AttributeError at runtime. The rewrite must NOT fire.
        let src = "extend str:\n    def shout(self) -> str:\n        return self.upper()\n\n\
                   greeting = \"hi\"\nprint(greeting.shout())\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(rewrites, 0);
    }

    #[test]
    fn rewrite_handles_parameter_annotation() {
        let src = "extend str:\n    def shout(self) -> str:\n        return self.upper()\n\n\
                   def greet(name: str) -> str:\n    return name.shout()\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(rewrites, 1);
        let out = tyc_emit::emit_python(&m);
        assert!(
            out.contains("__typhon_ext_str__shout__(name)"),
            "got:\n{out}"
        );
    }

    #[test]
    fn rewrite_handles_generic_list_annotation() {
        let src = "extend list:\n    def head(self) -> int:\n        return self[0]\n\n\
                   xs: list[int] = [1, 2, 3]\nprint(xs.head())\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(rewrites, 1);
    }

    #[test]
    fn rewrite_descends_into_binop_and_list_expressions() {
        // Coverage for nested expression contexts: a call inside a binop
        // and a call inside a list literal must both be rewritten.
        let src = "extend str:\n    def shout(self) -> str:\n        return self.upper()\n\n\
                   def f(name: str) -> str:\n    return name.shout() + \"!\"\n\n\
                   def g(name: str) -> list[str]:\n    return [name.shout()]\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(
            rewrites, 2,
            "expected both nested calls to be rewritten; got {rewrites}"
        );
    }

    #[test]
    fn rewrite_descends_into_for_while_with_try_match() {
        // The receiver is a parameter typed `str`, so the rewrite should
        // fire from every block-bearing statement variant.
        let src = "extend str:\n    def shout(self) -> str:\n        return self.upper()\n\n\
                   def run(name: str) -> None:\n    \
                   for _ in [1]:\n        print(name.shout())\n    \
                   while False:\n        print(name.shout())\n    \
                   with open('x') as _:\n        print(name.shout())\n    \
                   try:\n        print(name.shout())\n    except Exception:\n        \
                       print(name.shout())\n    \
                   match name:\n        case _:\n            print(name.shout())\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(
            rewrites, 6,
            "for/while/with/try-body/try-except/match should all rewrite; got {rewrites}"
        );
    }

    #[test]
    fn rewrite_picks_up_function_local_annotated_binding() {
        // A local `let s: str = ...` declaration inside a function body
        // must populate the local env in time for a subsequent call.
        let src = "extend str:\n    def shout(self) -> str:\n        return self.upper()\n\n\
                   def f() -> str:\n    s: str = \"hi\"\n    return s.shout()\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(rewrites, 1);
    }

    #[test]
    fn rewrite_does_not_leak_outer_annotation_into_function_scope() {
        // Module-level `name: str` is shadowed by a function-local
        // rebinding (no annotation, different runtime type). The
        // function must NOT inherit the outer annotation.
        let src = "extend str:\n    def shout(self) -> str:\n        return self.upper()\n\n\
                   name: str = \"global\"\n\n\
                   def f() -> str:\n    name = object()\n    return name.shout()\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(
            rewrites, 0,
            "function scope must not inherit outer module annotation; got {rewrites}"
        );
    }

    #[test]
    fn rewrite_handles_keyword_only_parameter_annotation() {
        let src = "extend str:\n    def shout(self) -> str:\n        return self.upper()\n\n\
                   def f(*, name: str) -> str:\n    return name.shout()\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(rewrites, 1);
    }

    #[test]
    fn rewrite_visits_elif_test_expression() {
        let src = "extend str:\n    def is_empty(self) -> bool:\n        return len(self) == 0\n\n\
                   def f(a: str, b: str) -> int:\n    \
                   if False:\n        return 0\n    elif b.is_empty():\n        return 1\n    \
                   return 2\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(rewrites, 1);
    }
}
