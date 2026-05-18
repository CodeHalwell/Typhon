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
    let module_env = collect_module_annotations(module);
    let mut rewrites = 0usize;
    let body = std::mem::take(&mut module.body);
    let mut new_body: Vec<Stmt> = Vec::with_capacity(body.len());
    for stmt in body {
        new_body.push(rewrite_stmt(stmt, registry, &module_env, &mut rewrites));
    }
    module.body = new_body;
    rewrites
}

/// Map from variable name to the built-in type it is annotated as, scoped
/// to the module's top-level. Function-local `let` annotations are tracked
/// separately by `rewrite_stmt` as it descends into bodies.
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

fn rewrite_stmt(stmt: Stmt, registry: &ExtensionRegistry, env: &Env, rewrites: &mut usize) -> Stmt {
    match stmt {
        Stmt::FunctionDef(mut f) => {
            let mut local = env.clone();
            for param in &f.parameters.args {
                if let Some(ann) = &param.parameter.annotation {
                    if let Some(ty) = annotation_to_type(ann) {
                        local.insert(param.parameter.name.as_str().to_owned(), ty);
                    }
                }
            }
            let body = std::mem::take(&mut f.body);
            let mut new_body: Vec<Stmt> = Vec::with_capacity(body.len());
            for s in body {
                new_body.push(rewrite_stmt(s, registry, &local, rewrites));
            }
            f.body = new_body;
            Stmt::FunctionDef(f)
        }
        Stmt::AnnAssign(mut a) => {
            if let Some(v) = a.value.as_mut() {
                rewrite_expr(v, registry, env, rewrites);
            }
            Stmt::AnnAssign(a)
        }
        Stmt::Assign(mut a) => {
            rewrite_expr(&mut a.value, registry, env, rewrites);
            Stmt::Assign(a)
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
        Stmt::If(mut i) => {
            rewrite_expr(&mut i.test, registry, env, rewrites);
            let body = std::mem::take(&mut i.body);
            i.body = body
                .into_iter()
                .map(|s| rewrite_stmt(s, registry, env, rewrites))
                .collect();
            for clause in &mut i.elif_else_clauses {
                let body = std::mem::take(&mut clause.body);
                clause.body = body
                    .into_iter()
                    .map(|s| rewrite_stmt(s, registry, env, rewrites))
                    .collect();
            }
            Stmt::If(i)
        }
        other => other,
    }
}

fn rewrite_expr(expr: &mut Expr, registry: &ExtensionRegistry, env: &Env, rewrites: &mut usize) {
    if let Expr::Call(call) = expr {
        // Recurse into arguments first.
        for arg in &mut call.arguments.args {
            rewrite_expr(arg, registry, env, rewrites);
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
                        }
                    }
                }
            }
        }
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
}
