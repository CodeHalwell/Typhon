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

use std::collections::HashMap;

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
fn annotate_self_param_with_builtin(
    f: &mut ruff_python_ast::StmtFunctionDef,
    builtin: &str,
) {
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

/// Free-function naming convention. `__typhon_ext_<TYPE>__<METHOD>`.
fn free_fn_name(ty: &str, method: &str) -> String {
    format!("__typhon_ext_{ty}__{method}")
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
    if registry.is_empty() {
        return 0;
    }
    let mut module_env = collect_module_annotations(module);
    let mut rewrites = 0usize;
    let body = std::mem::take(&mut module.body);
    let mut new_body: Vec<Stmt> = Vec::with_capacity(body.len());
    for stmt in body {
        new_body.push(rewrite_stmt(stmt, registry, &mut module_env, &mut rewrites));
    }
    module.body = new_body;
    rewrites
}

/// Map from variable name to the built-in type it is annotated as.
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
    registry: &ExtensionRegistry,
    env: &mut Env,
    rewrites: &mut usize,
) -> Stmt {
    match stmt {
        Stmt::FunctionDef(mut f) => {
            // Fresh scope: nothing from the enclosing block leaks in.
            let mut local = Env::new();
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
            f.body = walk_body(std::mem::take(&mut f.body), registry, &mut local, rewrites);
            Stmt::FunctionDef(f)
        }
        Stmt::ClassDef(mut c) => {
            // Class bodies also open a fresh scope.
            let mut local = Env::new();
            c.body = walk_body(std::mem::take(&mut c.body), registry, &mut local, rewrites);
            Stmt::ClassDef(c)
        }
        Stmt::AnnAssign(mut a) => {
            if let Some(v) = a.value.as_mut() {
                rewrite_expr(v, registry, env, rewrites);
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
            rewrite_expr(&mut a.value, registry, env, rewrites);
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
            rewrite_expr(&mut a.value, registry, env, rewrites);
            Stmt::AugAssign(a)
        }
        Stmt::Expr(mut e) => {
            rewrite_expr(&mut e.value, registry, env, rewrites);
            Stmt::Expr(e)
        }
        Stmt::Return(mut r) => {
            if let Some(v) = r.value.as_mut() {
                rewrite_expr(v, registry, env, rewrites);
            }
            Stmt::Return(r)
        }
        Stmt::Raise(mut r) => {
            if let Some(v) = r.exc.as_mut() {
                rewrite_expr(v, registry, env, rewrites);
            }
            if let Some(v) = r.cause.as_mut() {
                rewrite_expr(v, registry, env, rewrites);
            }
            Stmt::Raise(r)
        }
        Stmt::Delete(mut d) => {
            for t in &mut d.targets {
                rewrite_expr(t, registry, env, rewrites);
            }
            Stmt::Delete(d)
        }
        Stmt::If(mut i) => {
            rewrite_expr(&mut i.test, registry, env, rewrites);
            i.body = walk_body(std::mem::take(&mut i.body), registry, env, rewrites);
            for clause in &mut i.elif_else_clauses {
                if let Some(t) = clause.test.as_mut() {
                    rewrite_expr(t, registry, env, rewrites);
                }
                clause.body = walk_body(std::mem::take(&mut clause.body), registry, env, rewrites);
            }
            Stmt::If(i)
        }
        Stmt::While(mut w) => {
            rewrite_expr(&mut w.test, registry, env, rewrites);
            w.body = walk_body(std::mem::take(&mut w.body), registry, env, rewrites);
            w.orelse = walk_body(std::mem::take(&mut w.orelse), registry, env, rewrites);
            Stmt::While(w)
        }
        Stmt::For(mut f) => {
            rewrite_expr(&mut f.target, registry, env, rewrites);
            rewrite_expr(&mut f.iter, registry, env, rewrites);
            f.body = walk_body(std::mem::take(&mut f.body), registry, env, rewrites);
            f.orelse = walk_body(std::mem::take(&mut f.orelse), registry, env, rewrites);
            Stmt::For(f)
        }
        Stmt::With(mut w) => {
            for item in &mut w.items {
                rewrite_expr(&mut item.context_expr, registry, env, rewrites);
                if let Some(v) = item.optional_vars.as_mut() {
                    rewrite_expr(v, registry, env, rewrites);
                }
            }
            w.body = walk_body(std::mem::take(&mut w.body), registry, env, rewrites);
            Stmt::With(w)
        }
        Stmt::Try(mut t) => {
            t.body = walk_body(std::mem::take(&mut t.body), registry, env, rewrites);
            for handler in &mut t.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(h) = handler;
                if let Some(ty) = h.type_.as_mut() {
                    rewrite_expr(ty, registry, env, rewrites);
                }
                h.body = walk_body(std::mem::take(&mut h.body), registry, env, rewrites);
            }
            t.orelse = walk_body(std::mem::take(&mut t.orelse), registry, env, rewrites);
            t.finalbody = walk_body(std::mem::take(&mut t.finalbody), registry, env, rewrites);
            Stmt::Try(t)
        }
        Stmt::Match(mut m) => {
            rewrite_expr(&mut m.subject, registry, env, rewrites);
            for case in &mut m.cases {
                if let Some(g) = case.guard.as_mut() {
                    rewrite_expr(g, registry, env, rewrites);
                }
                case.body = walk_body(std::mem::take(&mut case.body), registry, env, rewrites);
            }
            Stmt::Match(m)
        }
        other => other,
    }
}

fn walk_body(
    body: Vec<Stmt>,
    registry: &ExtensionRegistry,
    env: &mut Env,
    rewrites: &mut usize,
) -> Vec<Stmt> {
    body.into_iter()
        .map(|s| rewrite_stmt(s, registry, env, rewrites))
        .collect()
}

/// Rewrite eligible attribute calls anywhere inside `expr`.  Descends
/// through every expression variant so a call buried in `x.shout() + "!"`
/// or `f(x.shout())` reaches the rewrite logic.
fn rewrite_expr(expr: &mut Expr, registry: &ExtensionRegistry, env: &Env, rewrites: &mut usize) {
    // 1. Try the local rewrite first when the head is a callable attribute
    //    on a typed receiver. Other variants fall through to (2) below.
    if let Expr::Call(call) = expr {
        // Recurse into call arguments and (for non-extension calls) the
        // callee expression itself so nested receivers still get fixed.
        for arg in &mut call.arguments.args {
            rewrite_expr(arg, registry, env, rewrites);
        }
        for kw in &mut call.arguments.keywords {
            rewrite_expr(&mut kw.value, registry, env, rewrites);
        }
        if let Expr::Attribute(attr) = call.func.as_ref() {
            if let Expr::Name(recv) = attr.value.as_ref() {
                if let Some(ty) = env.get(recv.id.as_str()) {
                    if let Some(methods) = registry.get(ty) {
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
                            return;
                        }
                    }
                }
            }
        }
        // Not an extension call — still descend into the callee so a
        // receiver buried inside a more complex call expression
        // (lambda, subscript, attribute chain) gets visited.
        rewrite_expr(&mut call.func, registry, env, rewrites);
        return;
    }

    // 2. Generic recursion through every Expr variant. The goal is
    //    coverage, not pretty matching — every shape that can contain
    //    a sub-expression descends.
    match expr {
        Expr::BoolOp(b) => {
            for v in &mut b.values {
                rewrite_expr(v, registry, env, rewrites);
            }
        }
        Expr::Named(n) => {
            rewrite_expr(&mut n.target, registry, env, rewrites);
            rewrite_expr(&mut n.value, registry, env, rewrites);
        }
        Expr::BinOp(b) => {
            rewrite_expr(&mut b.left, registry, env, rewrites);
            rewrite_expr(&mut b.right, registry, env, rewrites);
        }
        Expr::UnaryOp(u) => rewrite_expr(&mut u.operand, registry, env, rewrites),
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
            rewrite_expr(&mut l.body, registry, &local, rewrites);
        }
        Expr::If(i) => {
            rewrite_expr(&mut i.test, registry, env, rewrites);
            rewrite_expr(&mut i.body, registry, env, rewrites);
            rewrite_expr(&mut i.orelse, registry, env, rewrites);
        }
        Expr::Dict(d) => {
            for item in &mut d.items {
                if let Some(k) = item.key.as_mut() {
                    rewrite_expr(k, registry, env, rewrites);
                }
                rewrite_expr(&mut item.value, registry, env, rewrites);
            }
        }
        Expr::Set(s) => {
            for e in &mut s.elts {
                rewrite_expr(e, registry, env, rewrites);
            }
        }
        Expr::ListComp(lc) => {
            rewrite_expr(&mut lc.elt, registry, env, rewrites);
            for gen in &mut lc.generators {
                rewrite_expr(&mut gen.iter, registry, env, rewrites);
                for f in &mut gen.ifs {
                    rewrite_expr(f, registry, env, rewrites);
                }
            }
        }
        Expr::SetComp(c) => {
            rewrite_expr(&mut c.elt, registry, env, rewrites);
            for gen in &mut c.generators {
                rewrite_expr(&mut gen.iter, registry, env, rewrites);
                for f in &mut gen.ifs {
                    rewrite_expr(f, registry, env, rewrites);
                }
            }
        }
        Expr::DictComp(c) => {
            if let Some(k) = c.key.as_mut() {
                rewrite_expr(k, registry, env, rewrites);
            }
            rewrite_expr(&mut c.value, registry, env, rewrites);
            for gen in &mut c.generators {
                rewrite_expr(&mut gen.iter, registry, env, rewrites);
                for f in &mut gen.ifs {
                    rewrite_expr(f, registry, env, rewrites);
                }
            }
        }
        Expr::Generator(g) => {
            rewrite_expr(&mut g.elt, registry, env, rewrites);
            for gen in &mut g.generators {
                rewrite_expr(&mut gen.iter, registry, env, rewrites);
                for f in &mut gen.ifs {
                    rewrite_expr(f, registry, env, rewrites);
                }
            }
        }
        Expr::Await(a) => rewrite_expr(&mut a.value, registry, env, rewrites),
        Expr::Yield(y) => {
            if let Some(v) = y.value.as_mut() {
                rewrite_expr(v, registry, env, rewrites);
            }
        }
        Expr::YieldFrom(y) => rewrite_expr(&mut y.value, registry, env, rewrites),
        Expr::Compare(c) => {
            rewrite_expr(&mut c.left, registry, env, rewrites);
            for cmp in &mut c.comparators {
                rewrite_expr(cmp, registry, env, rewrites);
            }
        }
        Expr::Attribute(a) => rewrite_expr(&mut a.value, registry, env, rewrites),
        Expr::Subscript(s) => {
            rewrite_expr(&mut s.value, registry, env, rewrites);
            rewrite_expr(&mut s.slice, registry, env, rewrites);
        }
        Expr::Starred(s) => rewrite_expr(&mut s.value, registry, env, rewrites),
        Expr::List(l) => {
            for e in &mut l.elts {
                rewrite_expr(e, registry, env, rewrites);
            }
        }
        Expr::Tuple(t) => {
            for e in &mut t.elts {
                rewrite_expr(e, registry, env, rewrites);
            }
        }
        Expr::Slice(s) => {
            if let Some(v) = s.lower.as_mut() {
                rewrite_expr(v, registry, env, rewrites);
            }
            if let Some(v) = s.upper.as_mut() {
                rewrite_expr(v, registry, env, rewrites);
            }
            if let Some(v) = s.step.as_mut() {
                rewrite_expr(v, registry, env, rewrites);
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
            Some("__typhon_ext_str__shout")
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
            s, Stmt::FunctionDef(f) if f.name.as_str() == "__typhon_ext_str__shout"
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
            out.contains("__typhon_ext_str__shout(greeting)"),
            "expected call-site rewrite; got:\n{out}"
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
        assert!(out.contains("__typhon_ext_str__shout(name)"), "got:\n{out}");
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
