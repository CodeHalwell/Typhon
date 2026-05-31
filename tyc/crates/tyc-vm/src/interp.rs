//! Tree-walking interpreter — the heart of the VM.
//!
//! Walks the post-preprocess Typhon AST (which is just Python AST with `let`
//! / `mut` markers and a handful of injected sugar like `__typhon_Err__`).
//! The interpreter is deliberately straightforward: no JIT, no bytecode, no
//! cross-module incremental compilation. It exists so `tyc run foo.ty`
//! produces output without ever touching a `python` interpreter.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use indexmap::IndexMap;
use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{Signed, ToPrimitive, Zero};
use ruff_python_ast::{
    self as ast, BoolOp, CmpOp, ExceptHandler, Expr, FStringPart, InterpolatedStringElement,
    ModModule, Mutability, Number, Operator, Parameters, Pattern, Stmt, UnaryOp,
};

use crate::env::{Env, EnvRef};
use crate::error::{
    attribute_error, index_error, key_error, name_error, not_implemented, type_error, value_error,
    vm_unsupported_use_compile, zero_division, Unwind, VmException,
};
use crate::value::{
    bigint_to_f64, Class, ClassField, DictMap, Function, HashKey, Instance, IterState, NativeFn,
    Value,
};

pub struct Interpreter {
    pub root: EnvRef,
    pub stack_depth: usize,
    pub max_stack_depth: usize,
    /// `sys.argv` for the running script. `argv[0]` is conventionally the
    /// script path; `argv[1..]` are the user-supplied arguments. Populated
    /// by `lib::run_*`, not by the host process's own argv.
    pub script_argv: Vec<String>,
    /// Directory holding sibling `.ty` source files for cross-module
    /// imports. When set, `from .repo import X` (and bare `import repo`)
    /// resolves against `<source_root>/repo.ty`. Without this, multi-file
    /// projects can only be run via `tyc run --compile`.
    pub source_root: Option<std::path::PathBuf>,
    /// Cache of loaded user modules keyed by their package-qualified
    /// name (e.g. "repo" or "sub.util"). Lets the same module be imported
    /// from multiple files without re-evaluating its body.
    pub module_cache: HashMap<String, Value>,
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}

impl Interpreter {
    pub fn new() -> Self {
        let root = Env::new_root();
        let mut interp = Interpreter {
            root: root.clone(),
            stack_depth: 0,
            // Match CPython's default `sys.getrecursionlimit()` of 1000
            // (FINDINGS #31). The tree-walking interpreter still pays a
            // real Rust stack frame for each Typhon frame, so values much
            // beyond this risk overflowing the OS thread stack rather
            // than hitting our explicit guard. If the host process has a
            // smaller stack, `tyc run` should be invoked with
            // `RUST_MIN_STACK` set or a dedicated thread with a larger
            // stack; a true fix would use `stacker::maybe_grow` but
            // `stacker` isn't a workspace dep yet — TODO.
            max_stack_depth: 1000,
            script_argv: Vec::new(),
            source_root: None,
            module_cache: HashMap::new(),
        };
        crate::builtins::install(&mut interp);
        interp
    }

    /// Entry point — run a parsed module's body in the root scope.
    pub fn run_module(&mut self, module: &ModModule) -> Result<(), Unwind> {
        let env = self.root.clone();
        self.exec_block(&module.body, &env)
    }

    // ── Statement evaluation ───────────────────────────────────────────────

    pub fn exec_block(&mut self, body: &[Stmt], env: &EnvRef) -> Result<(), Unwind> {
        for stmt in body {
            self.exec_stmt(stmt, env)?;
        }
        Ok(())
    }

    fn exec_stmt(&mut self, stmt: &Stmt, env: &EnvRef) -> Result<(), Unwind> {
        match stmt {
            Stmt::Expr(e) => {
                self.eval_expr(&e.value, env)?;
                Ok(())
            }
            Stmt::Pass(_) => Ok(()),
            Stmt::Break(_) => Err(Unwind::Break),
            Stmt::Continue(_) => Err(Unwind::Continue),
            Stmt::Return(r) => {
                let v = match &r.value {
                    Some(e) => self.eval_expr(e, env)?,
                    None => Value::None,
                };
                Err(Unwind::Return(v))
            }
            Stmt::Assign(a) => {
                let value = self.eval_expr(&a.value, env)?;
                for target in &a.targets {
                    self.assign_target(target, value.clone(), env, a.mutability)?;
                }
                Ok(())
            }
            Stmt::AnnAssign(a) => {
                if let Some(val_expr) = &a.value {
                    let value = self.eval_expr(val_expr, env)?;
                    self.assign_target(&a.target, value, env, a.mutability)?;
                } else {
                    // Bare annotation in a class body — record as a declared
                    // field. In a function body it has no runtime effect.
                    if let Expr::Name(n) = a.target.as_ref() {
                        // Reserve the slot if it isn't there; this lets class
                        // bodies see annotated fields when constructing the
                        // synthesised __init__.
                        if env.get(n.id.as_str()).is_none() {
                            // For module / class scope, the class-body sweep
                            // picks fields up directly from the AST, so we
                            // don't need to do anything here.
                        }
                    }
                }
                Ok(())
            }
            Stmt::AugAssign(a) => {
                let current = self.eval_expr(&a.target, env)?;
                let rhs = self.eval_expr(&a.value, env)?;
                let new = self.binop(&current, a.op, &rhs)?;
                self.assign_target(&a.target, new, env, None)?;
                Ok(())
            }
            Stmt::If(s) => {
                let cond = self.eval_expr(&s.test, env)?;
                if cond.truthy() {
                    self.exec_block(&s.body, env)?;
                } else {
                    for clause in &s.elif_else_clauses {
                        match &clause.test {
                            Some(test) => {
                                let c = self.eval_expr(test, env)?;
                                if c.truthy() {
                                    self.exec_block(&clause.body, env)?;
                                    return Ok(());
                                }
                            }
                            None => {
                                self.exec_block(&clause.body, env)?;
                                return Ok(());
                            }
                        }
                    }
                }
                Ok(())
            }
            Stmt::While(s) => {
                let mut completed = true;
                loop {
                    let cond = self.eval_expr(&s.test, env)?;
                    if !cond.truthy() {
                        break;
                    }
                    match self.exec_block(&s.body, env) {
                        Ok(()) => {}
                        Err(Unwind::Continue) => continue,
                        Err(Unwind::Break) => {
                            completed = false;
                            break;
                        }
                        Err(e) => return Err(e),
                    }
                }
                if completed && !s.orelse.is_empty() {
                    self.exec_block(&s.orelse, env)?;
                }
                Ok(())
            }
            Stmt::For(s) => {
                if s.is_async {
                    return Err(vm_unsupported_use_compile("async for"));
                }
                let iterable = self.eval_expr(&s.iter, env)?;
                let iter = self.make_iter(iterable)?;
                let mut completed = true;
                loop {
                    let next = self.iter_next(&iter)?;
                    let Some(v) = next else { break };
                    self.assign_target(&s.target, v, env, None)?;
                    match self.exec_block(&s.body, env) {
                        Ok(()) => {}
                        Err(Unwind::Continue) => continue,
                        Err(Unwind::Break) => {
                            completed = false;
                            break;
                        }
                        Err(e) => return Err(e),
                    }
                }
                if completed && !s.orelse.is_empty() {
                    self.exec_block(&s.orelse, env)?;
                }
                Ok(())
            }
            Stmt::FunctionDef(f) => {
                if f.is_async {
                    return Err(vm_unsupported_use_compile("async functions"));
                }
                let func = self.build_function(f, env)?;
                // Run decorators in reverse order (innermost first), matching Python.
                let mut value = Value::Function(Rc::new(func));
                for deco in f.decorator_list.iter().rev() {
                    value = self.apply_decorator(deco, value, env)?;
                }
                env.set(f.name.as_str(), value);
                Ok(())
            }
            Stmt::ClassDef(c) => {
                let name = c.name.as_str();
                // Synthesised impl-block: `class __typhon_impl_Foo(object):` —
                // merge its method block into the already-declared `Foo`
                // rather than registering a separate class. This mirrors what
                // `tyc-desugar` does for the compiled path.
                if let Some(target) = name.strip_prefix("__typhon_impl_") {
                    if let Some(Value::Class(existing)) = env.get(target) {
                        let new_class = self.build_class(c, env)?;
                        for (name, m) in new_class.methods.borrow().iter() {
                            existing
                                .methods
                                .borrow_mut()
                                .insert(name.clone(), m.clone());
                        }
                        for (k, v) in new_class.class_attrs.borrow().iter() {
                            existing
                                .class_attrs
                                .borrow_mut()
                                .insert(k.clone(), v.clone());
                        }
                        return Ok(());
                    }
                    // No target — fall through and register as a regular class.
                }
                let class = self.build_class(c, env)?;
                env.set(name, Value::Class(class));
                Ok(())
            }
            Stmt::Import(im) => {
                for alias in &im.names {
                    // Python: `import a.b.c` binds the *root* name `a` to
                    // the root package; later `a.b.c.x` accesses traverse
                    // through attribute lookups. `import a.b as foo`
                    // binds `foo` to the submodule. Mirror that — when
                    // an `as` clause is present, load and bind the
                    // dotted name; otherwise resolve to the root module.
                    let (target, bind) = match &alias.asname {
                        Some(i) => (alias.name.as_str().to_owned(), i.as_str().to_owned()),
                        None => {
                            let root = alias
                                .name
                                .as_str()
                                .split('.')
                                .next()
                                .unwrap_or("")
                                .to_owned();
                            (root.clone(), root)
                        }
                    };
                    let module = self.import_module(&target)?;
                    env.set(&bind, module);
                }
                Ok(())
            }
            Stmt::ImportFrom(im) => {
                let module_name = im
                    .module
                    .as_ref()
                    .map(|i| i.as_str().to_owned())
                    .unwrap_or_default();
                // Relative imports (`from .repo import X`) carry a `level`
                // > 0. The dot-count tells us how many package levels up
                // to start from. The VM treats the configured source_root
                // as the package root, so any positive level resolves the
                // import relative to it. This is enough for the common
                // pattern of `from .sibling import x` between files
                // sharing a `src/` directory.
                let module = if im.level > 0 {
                    if module_name.is_empty() {
                        // `from . import sibling` — load each name as a
                        // sibling module instead of going through an
                        // umbrella package object.
                        for alias in &im.names {
                            let attr = alias.name.as_str();
                            let val = self.import_module(attr)?;
                            let bind = alias
                                .asname
                                .as_ref()
                                .map(|i| i.as_str().to_owned())
                                .unwrap_or_else(|| attr.to_owned());
                            env.set(&bind, val);
                        }
                        return Ok(());
                    }
                    self.import_module(&module_name)?
                } else {
                    self.import_module(&module_name)?
                };
                for alias in &im.names {
                    let attr = alias.name.as_str();
                    let val = self.get_attr(&module, attr)?;
                    let bind = alias
                        .asname
                        .as_ref()
                        .map(|i| i.as_str().to_owned())
                        .unwrap_or_else(|| attr.to_owned());
                    env.set(&bind, val);
                }
                Ok(())
            }
            Stmt::Raise(r) => {
                let exc = match &r.exc {
                    Some(e) => self.eval_expr(e, env)?,
                    None => {
                        return Err(Unwind::Exception(VmException::new(
                            "RuntimeError",
                            "No active exception to re-raise",
                        )))
                    }
                };
                Err(self.value_to_exception(exc))
            }
            Stmt::Try(t) => self.exec_try(t, env),
            Stmt::Match(m) => self.exec_match(m, env),
            Stmt::Assert(a) => {
                let cond = self.eval_expr(&a.test, env)?;
                if !cond.truthy() {
                    let msg = match &a.msg {
                        Some(m) => self.eval_expr(m, env)?.py_str(),
                        None => "".to_string(),
                    };
                    return Err(Unwind::Exception(VmException::new("AssertionError", msg)));
                }
                Ok(())
            }
            Stmt::Global(g) => {
                for name in &g.names {
                    env.declare_global(name.as_str());
                }
                Ok(())
            }
            Stmt::Nonlocal(n) => {
                for name in &n.names {
                    env.declare_nonlocal(name.as_str());
                }
                Ok(())
            }
            Stmt::Delete(d) => {
                for t in &d.targets {
                    match t {
                        Expr::Name(n) => {
                            env.delete(n.id.as_str());
                        }
                        Expr::Subscript(sub) => {
                            let target = self.eval_expr(&sub.value, env)?;
                            let key = self.eval_expr(&sub.slice, env)?;
                            self.del_subscript(&target, &key)?;
                        }
                        _ => return Err(not_implemented("complex delete targets")),
                    }
                }
                Ok(())
            }
            Stmt::With(w) => self.exec_with(w, env),
            Stmt::TypeAlias(_) => Ok(()),
            Stmt::IpyEscapeCommand(_) => Err(not_implemented("IPython escape commands")),
        }
    }

    // ── Function/class construction ────────────────────────────────────────

    fn build_function(
        &mut self,
        f: &ast::StmtFunctionDef,
        env: &EnvRef,
    ) -> Result<Function, Unwind> {
        // Evaluate default values at def-time, in source order.
        let mut defaults = Vec::new();
        for p in f.parameters.iter_non_variadic_params() {
            let v = match p.default() {
                Some(d) => Some(self.eval_expr(d, env)?),
                None => None,
            };
            defaults.push(v);
        }
        Ok(Function {
            name: f.name.as_str().to_owned(),
            params: f.parameters.clone(),
            body: Rc::new(f.body.clone()),
            defaults,
            closure: env.clone(),
            is_async: f.is_async,
        })
    }

    fn build_class(&mut self, c: &ast::StmtClassDef, env: &EnvRef) -> Result<Rc<Class>, Unwind> {
        // Resolve base classes.
        let mut bases = Vec::new();
        if let Some(args) = &c.arguments {
            for arg in args.args.iter() {
                let v = self.eval_expr(arg, env)?;
                match v {
                    Value::Class(c) => bases.push(c),
                    Value::Module(_) => {
                        // e.g. `typing.Protocol` referenced as `Protocol` — ignored for v1.
                    }
                    _ => {
                        // Builtin marker classes like `Protocol`, `BaseModel`
                        // — for v1 we ignore non-Class bases.
                    }
                }
            }
        }

        // Walk the class body in a temporary scope so any non-fn statements
        // (constants, comprehensions used as defaults) get evaluated.
        let body_env = Env::new_child(env);

        let mut fields = Vec::new();
        let mut methods: HashMap<String, Rc<Function>> = HashMap::new();
        let mut class_attrs: HashMap<String, Value> = HashMap::new();
        let mut properties: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut classmethods: std::collections::HashSet<String> = std::collections::HashSet::new();

        for stmt in &c.body {
            match stmt {
                Stmt::FunctionDef(f) => {
                    let func = self.build_function(f, &body_env)?;
                    let mut v = Value::Function(Rc::new(func));
                    let has_deco = |want: &str| {
                        f.decorator_list.iter().any(|d| {
                            matches!(&d.expression, Expr::Name(n) if n.id.as_str() == want)
                        })
                    };
                    let is_property = has_deco("property");
                    let is_classmethod = has_deco("classmethod");
                    for deco in f.decorator_list.iter().rev() {
                        v = self.apply_decorator(deco, v, &body_env)?;
                    }
                    if let Value::Function(f) = &v {
                        if is_property {
                            properties.insert(f.name.clone());
                        }
                        if is_classmethod {
                            classmethods.insert(f.name.clone());
                        }
                        methods.insert(f.name.clone(), f.clone());
                    } else {
                        class_attrs.insert(f.name.as_str().into(), v);
                    }
                }
                Stmt::AnnAssign(a) => {
                    if let Expr::Name(n) = a.target.as_ref() {
                        let default = match &a.value {
                            Some(e) => Some(self.eval_expr(e, &body_env)?),
                            None => None,
                        };
                        fields.push(ClassField {
                            name: n.id.as_str().to_owned(),
                            default,
                        });
                    }
                }
                Stmt::Assign(a) => {
                    let v = self.eval_expr(&a.value, &body_env)?;
                    for t in &a.targets {
                        if let Expr::Name(n) = t {
                            class_attrs.insert(n.id.as_str().to_owned(), v.clone());
                        }
                    }
                }
                Stmt::Pass(_) => {}
                Stmt::Expr(_) => {} // docstrings etc.
                _ => {}
            }
        }

        // Inherit methods from base classes that aren't already overridden.
        for base in &bases {
            for (name, m) in base.methods.borrow().iter() {
                methods.entry(name.clone()).or_insert_with(|| m.clone());
            }
            for (name, v) in base.class_attrs.borrow().iter() {
                class_attrs.entry(name.clone()).or_insert_with(|| v.clone());
            }
            for name in base.properties.borrow().iter() {
                properties.insert(name.clone());
            }
            for name in base.classmethods.borrow().iter() {
                classmethods.insert(name.clone());
            }
        }

        Ok(Rc::new(Class {
            name: c.name.as_str().to_owned(),
            methods: RefCell::new(methods),
            fields,
            class_attrs: RefCell::new(class_attrs),
            bases,
            properties: RefCell::new(properties),
            classmethods: RefCell::new(classmethods),
        }))
    }

    fn apply_decorator(
        &mut self,
        deco: &ast::Decorator,
        target: Value,
        env: &EnvRef,
    ) -> Result<Value, Unwind> {
        // Recognise a few well-known decorators that we either no-op
        // (validation-only) or implement natively.
        if let Some(name) = decorator_simple_name(&deco.expression) {
            match name.as_str() {
                // Validation-only — emitted code paths handle these statically.
                "pure" | "dataclass" | "gatherable" | "override" | "final" | "staticmethod"
                | "classmethod" => return Ok(target),
                "memo" | "cache" | "lru_cache" => {
                    return Ok(self.wrap_memo(target));
                }
                _ => {}
            }
        }
        // Generic path: call the decorator value.
        let f = self.eval_expr(&deco.expression, env)?;
        self.call_value(f, vec![target], &[])
    }

    fn wrap_memo(&mut self, target: Value) -> Value {
        let cache: Rc<RefCell<HashMap<HashKey, Value>>> = Rc::new(RefCell::new(HashMap::new()));
        let inner = target;
        let func = NativeFn::new("memo", move |interp, args| {
            // Hash all positional arguments together.
            let mut keys = Vec::with_capacity(args.len());
            for a in &args {
                keys.push(a.to_hash_key()?);
            }
            let key = HashKey::Tuple(Rc::new(keys));
            if let Some(v) = cache.borrow().get(&key).cloned() {
                return Ok(v);
            }
            let result = interp.call_value(inner.clone(), args, &[])?;
            cache.borrow_mut().insert(key, result.clone());
            Ok(result)
        });
        Value::Native(Rc::new(func))
    }

    // ── Imports ────────────────────────────────────────────────────────────

    fn import_module(&mut self, name: &str) -> Result<Value, Unwind> {
        // Cache hit: same name imported earlier in the same VM run.
        if let Some(cached) = self.module_cache.get(name).cloned() {
            return Ok(cached);
        }
        // Try the host stdlib / typhon_runtime first so user code can't
        // accidentally shadow `import json` with a sibling json.ty (the
        // stdlib_module_shadow lint already nudges users away from this
        // at check time).
        match crate::builtins::resolve_module(self, name) {
            Ok(v) => {
                self.module_cache.insert(name.to_owned(), v.clone());
                Ok(v)
            }
            Err(stdlib_err) => {
                // Fall back to loading a sibling Typhon source file.
                if let Some(loaded) = self.try_load_typhon_module(name)? {
                    self.module_cache.insert(name.to_owned(), loaded.clone());
                    return Ok(loaded);
                }
                Err(stdlib_err)
            }
        }
    }

    /// Resolve `name` (possibly dotted) against the configured source
    /// root. Returns `Ok(Some(...))` on a successful load, `Ok(None)` if
    /// no matching `.ty` file exists, and `Err` if a file exists but
    /// failed to parse or evaluate.
    fn try_load_typhon_module(&mut self, name: &str) -> Result<Option<Value>, Unwind> {
        let Some(root) = self.source_root.clone() else {
            return Ok(None);
        };
        // `foo.bar.baz` → `foo/bar/baz.ty` or `foo/bar/baz/__init__.ty`
        let rel = name.replace('.', std::path::MAIN_SEPARATOR_STR);
        let candidates = [
            root.join(format!("{rel}.ty")),
            root.join(&rel).join("__init__.ty"),
        ];
        let path = candidates.iter().find(|p| p.exists()).cloned();
        let Some(path) = path else {
            return Ok(None);
        };

        let source = std::fs::read_to_string(&path).map_err(|e| {
            crate::error::Unwind::Exception(crate::error::VmException::new(
                "ImportError",
                format!("cannot read '{}': {e}", path.display()),
            ))
        })?;

        // Run the same preprocess + desugar pipeline lib.rs uses for the
        // top-level entry, so the imported module sees identical surface
        // syntax handling.
        use tyc_syntax::preprocess;
        let expanded = preprocess::expand_question_ops(&preprocess::expand_pipes(
            &preprocess::expand_with_chains(&preprocess::expand_go_calls(
                &preprocess::expand_gather_blocks(&preprocess::expand_multiline_guards(
                    &preprocess::expand_typed_let_unpack(&preprocess::expand_lazy_lets(&source)),
                )),
            )),
        ));
        let prep = preprocess::preprocess(&expanded);
        let parsed = tyc_syntax::parse_module(&prep.python_source).map_err(|e| {
            crate::error::Unwind::Exception(crate::error::VmException::new(
                "ImportError",
                format!("parse error in '{}': {e}", path.display()),
            ))
        })?;
        let mut module = parsed.into_syntax();
        let (comptime_values, _diags) = tyc_analyse::evaluate_comptime_with_functions(
            &module,
            &prep.comptime_bindings,
            &prep.comptime_functions,
        );
        module = tyc_analyse::substitute_comptime_literals(
            module,
            &comptime_values,
            &prep.comptime_functions,
        );
        let desugar_out = tyc_desugar::desugar_module(&module);
        module = desugar_out.module;
        let (registry, _stats) = tyc_analyse::extract_builtin_extensions(&mut module);
        let _ = tyc_analyse::rewrite_builtin_extension_calls(&mut module, &registry);

        // Evaluate the module body in a fresh child scope of root; copy
        // every named binding into a Module namespace so attribute
        // lookups resolve to user functions / classes / constants.
        let module_env = Env::new_child(&self.root);
        self.exec_block(&module.body, &module_env)?;

        use crate::value::Module;
        let mut members: HashMap<String, Value> = HashMap::new();
        for (k, v) in module_env.snapshot().into_iter() {
            members.insert(k, v);
        }
        Ok(Some(Value::Module(Rc::new(Module {
            name: name.to_owned(),
            members: RefCell::new(members),
        }))))
    }

    // ── Expression evaluation ──────────────────────────────────────────────

    pub fn eval_expr(&mut self, expr: &Expr, env: &EnvRef) -> Result<Value, Unwind> {
        match expr {
            Expr::NumberLiteral(n) => Ok(number_to_value(&n.value)),
            Expr::StringLiteral(s) => Ok(Value::Str(Rc::new(s.value.to_str().to_owned()))),
            Expr::BytesLiteral(b) => {
                let mut buf = Vec::new();
                for part in b.value.iter() {
                    buf.extend_from_slice(&part.value);
                }
                Ok(Value::Bytes(Rc::new(buf)))
            }
            Expr::BooleanLiteral(b) => Ok(Value::Bool(b.value)),
            Expr::NoneLiteral(_) => Ok(Value::None),
            Expr::EllipsisLiteral(_) => Err(not_implemented("Ellipsis literal")),
            Expr::FString(f) => self.eval_fstring(f, env),
            Expr::Name(n) => env
                .get(n.id.as_str())
                .ok_or_else(|| name_error(format!("name '{}' is not defined", n.id.as_str()))),
            Expr::BinOp(b) => {
                let left = self.eval_expr(&b.left, env)?;
                let right = self.eval_expr(&b.right, env)?;
                self.binop(&left, b.op, &right)
            }
            Expr::UnaryOp(u) => {
                let operand = self.eval_expr(&u.operand, env)?;
                self.unop(u.op, &operand)
            }
            Expr::BoolOp(b) => {
                let mut last = Value::None;
                for (i, v) in b.values.iter().enumerate() {
                    let val = self.eval_expr(v, env)?;
                    match b.op {
                        BoolOp::And => {
                            if !val.truthy() {
                                return Ok(val);
                            }
                            last = val;
                        }
                        BoolOp::Or => {
                            if val.truthy() {
                                return Ok(val);
                            }
                            last = val;
                        }
                    }
                    if i == b.values.len() - 1 {
                        return Ok(last);
                    }
                }
                Ok(last)
            }
            Expr::Compare(c) => self.eval_compare(c, env),
            Expr::Call(c) => self.eval_call(c, env),
            Expr::Attribute(a) => {
                let receiver = self.eval_expr(&a.value, env)?;
                self.get_attr(&receiver, a.attr.as_str())
            }
            Expr::Subscript(s) => {
                let target = self.eval_expr(&s.value, env)?;
                let slice = self.eval_subscript_index(&s.slice, env)?;
                self.subscript(&target, &slice)
            }
            Expr::List(l) => {
                let mut items = Vec::with_capacity(l.elts.len());
                for e in &l.elts {
                    self.eval_unpackable(e, env, &mut items)?;
                }
                Ok(Value::List(Rc::new(RefCell::new(items))))
            }
            Expr::Tuple(t) => {
                let mut items = Vec::with_capacity(t.elts.len());
                for e in &t.elts {
                    self.eval_unpackable(e, env, &mut items)?;
                }
                Ok(Value::Tuple(Rc::new(items)))
            }
            Expr::Set(s) => {
                let mut set = std::collections::HashSet::new();
                for e in &s.elts {
                    let v = self.eval_expr(e, env)?;
                    set.insert(v.to_hash_key()?);
                }
                Ok(Value::Set(Rc::new(RefCell::new(set))))
            }
            Expr::Dict(d) => {
                let mut map: DictMap = IndexMap::new();
                for item in &d.items {
                    match (&item.key, &item.value) {
                        (Some(k), v) => {
                            let key = self.eval_expr(k, env)?.to_hash_key()?;
                            map.insert(key, self.eval_expr(v, env)?);
                        }
                        (None, v) => {
                            // {**other}
                            let other = self.eval_expr(v, env)?;
                            match other {
                                Value::Dict(d) => {
                                    for (k, v) in d.borrow().iter() {
                                        map.insert(k.clone(), v.clone());
                                    }
                                }
                                _ => return Err(type_error("** unpack expected a mapping")),
                            }
                        }
                    }
                }
                Ok(Value::Dict(Rc::new(RefCell::new(map))))
            }
            Expr::If(t) => {
                let cond = self.eval_expr(&t.test, env)?;
                if cond.truthy() {
                    self.eval_expr(&t.body, env)
                } else {
                    self.eval_expr(&t.orelse, env)
                }
            }
            Expr::Named(n) => {
                let v = self.eval_expr(&n.value, env)?;
                if let Expr::Name(name) = n.target.as_ref() {
                    env.assign_or_create(name.id.as_str(), v.clone());
                }
                Ok(v)
            }
            Expr::Starred(_) => Err(type_error("can't use starred expression here")),
            Expr::Slice(_) => Err(type_error("slice expression outside subscript")),
            Expr::Lambda(l) => {
                let params = l.parameters.clone().unwrap_or_else(|| {
                    Box::new(Parameters {
                        range: l.range,
                        node_index: Default::default(),
                        posonlyargs: vec![],
                        args: vec![],
                        vararg: None,
                        kwonlyargs: vec![],
                        kwarg: None,
                    })
                });
                let body_stmt = Stmt::Return(ast::StmtReturn {
                    node_index: Default::default(),
                    range: l.range,
                    value: Some(l.body.clone()),
                });
                let func = Function {
                    name: "<lambda>".into(),
                    params,
                    body: Rc::new(vec![body_stmt]),
                    defaults: vec![],
                    closure: env.clone(),
                    is_async: false,
                };
                Ok(Value::Function(Rc::new(func)))
            }
            Expr::ListComp(c) => self.eval_listcomp(c, env),
            Expr::SetComp(c) => self.eval_setcomp(c, env),
            Expr::DictComp(c) => self.eval_dictcomp(c, env),
            Expr::Generator(g) => {
                // Materialise as a list — true lazy generators are out of scope for v1.
                let listy = ast::ExprListComp {
                    node_index: g.node_index.clone(),
                    range: g.range,
                    elt: g.elt.clone(),
                    generators: g.generators.clone(),
                };
                self.eval_listcomp(&listy, env)
            }
            Expr::Await(_) => Err(vm_unsupported_use_compile("await expressions")),
            Expr::Yield(_) | Expr::YieldFrom(_) => Err(vm_unsupported_use_compile(
                "generators (`yield` / `yield from`)",
            )),
            Expr::TString(_) => Err(not_implemented("template strings")),
            Expr::IpyEscapeCommand(_) => Err(not_implemented("IPython escape commands")),
        }
    }

    fn eval_unpackable(
        &mut self,
        e: &Expr,
        env: &EnvRef,
        out: &mut Vec<Value>,
    ) -> Result<(), Unwind> {
        if let Expr::Starred(s) = e {
            let v = self.eval_expr(&s.value, env)?;
            let iter = self.make_iter(v)?;
            while let Some(x) = self.iter_next(&iter)? {
                out.push(x);
            }
            Ok(())
        } else {
            out.push(self.eval_expr(e, env)?);
            Ok(())
        }
    }

    fn eval_subscript_index(&mut self, expr: &Expr, env: &EnvRef) -> Result<Value, Unwind> {
        if let Expr::Slice(s) = expr {
            let lower = match &s.lower {
                Some(e) => self.eval_expr(e, env)?,
                None => Value::None,
            };
            let upper = match &s.upper {
                Some(e) => self.eval_expr(e, env)?,
                None => Value::None,
            };
            let step = match &s.step {
                Some(e) => self.eval_expr(e, env)?,
                None => Value::None,
            };
            Ok(Value::Tuple(Rc::new(vec![
                Value::Str(Rc::new("__slice__".into())),
                lower,
                upper,
                step,
            ])))
        } else {
            self.eval_expr(expr, env)
        }
    }

    fn eval_fstring(&mut self, f: &ast::ExprFString, env: &EnvRef) -> Result<Value, Unwind> {
        let mut out = String::new();
        for part in f.value.iter() {
            match part {
                FStringPart::Literal(lit) => out.push_str(&lit.value),
                FStringPart::FString(fs) => {
                    for elt in fs.elements.iter() {
                        match elt {
                            InterpolatedStringElement::Literal(lit) => out.push_str(&lit.value),
                            InterpolatedStringElement::Interpolation(interp) => {
                                let v = self.eval_expr(&interp.expression, env)?;
                                let s = match interp.conversion {
                                    ast::ConversionFlag::Repr => self.repr_of(&v)?,
                                    ast::ConversionFlag::Str
                                    | ast::ConversionFlag::None
                                    | ast::ConversionFlag::Ascii => self.str_of(&v)?,
                                };
                                // Format spec: limited support — width / precision for floats only.
                                if let Some(spec) = &interp.format_spec {
                                    let spec_str = self.format_spec_text(spec, env)?;
                                    out.push_str(&format_with_spec(&v, &s, &spec_str)?);
                                } else {
                                    out.push_str(&s);
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(Value::Str(Rc::new(out)))
    }

    fn format_spec_text(
        &mut self,
        spec: &ast::InterpolatedStringFormatSpec,
        env: &EnvRef,
    ) -> Result<String, Unwind> {
        let mut out = String::new();
        for elt in spec.elements.iter() {
            match elt {
                InterpolatedStringElement::Literal(lit) => out.push_str(&lit.value),
                InterpolatedStringElement::Interpolation(interp) => {
                    let v = self.eval_expr(&interp.expression, env)?;
                    out.push_str(&v.py_str());
                }
            }
        }
        Ok(out)
    }

    fn eval_compare(&mut self, c: &ast::ExprCompare, env: &EnvRef) -> Result<Value, Unwind> {
        let mut left = self.eval_expr(&c.left, env)?;
        for (op, right_expr) in c.ops.iter().zip(c.comparators.iter()) {
            let right = self.eval_expr(right_expr, env)?;
            let r = self.cmp_op(*op, &left, &right)?;
            if !r {
                return Ok(Value::Bool(false));
            }
            left = right;
        }
        Ok(Value::Bool(true))
    }

    /// Invoke a single-argument rich-comparison dunder (`__eq__`, `__lt__`, …)
    /// on an instance operand, returning the boolean truthiness if one exists.
    fn cmp_dunder(&mut self, op: CmpOp, l: &Value, r: &Value) -> Result<Option<bool>, Unwind> {
        let (name, rname): (&str, &str) = match op {
            CmpOp::Eq => ("__eq__", "__eq__"),
            CmpOp::NotEq => ("__ne__", "__ne__"),
            CmpOp::Lt => ("__lt__", "__gt__"),
            CmpOp::LtE => ("__le__", "__ge__"),
            CmpOp::Gt => ("__gt__", "__lt__"),
            CmpOp::GtE => ("__ge__", "__le__"),
            _ => return Ok(None),
        };
        if let Value::Instance(i) = l {
            if let Some(m) = self.find_method(&i.class, name) {
                let res = self.call_value(
                    Value::BoundMethod {
                        receiver: Box::new(l.clone()),
                        function: m,
                    },
                    vec![r.clone()],
                    &[],
                )?;
                return Ok(Some(res.truthy()));
            }
        }
        // Reflected comparison on the right instance operand.
        if let Value::Instance(i) = r {
            if let Some(m) = self.find_method(&i.class, rname) {
                let res = self.call_value(
                    Value::BoundMethod {
                        receiver: Box::new(r.clone()),
                        function: m,
                    },
                    vec![l.clone()],
                    &[],
                )?;
                return Ok(Some(res.truthy()));
            }
        }
        Ok(None)
    }

    fn cmp_op(&mut self, op: CmpOp, l: &Value, r: &Value) -> Result<bool, Unwind> {
        use std::cmp::Ordering::*;
        // User-defined rich comparisons take priority when an operand is a
        // class instance (Python's `__eq__` / `__lt__` / … protocol).
        if matches!(l, Value::Instance(_)) || matches!(r, Value::Instance(_)) {
            if let Some(b) = self.cmp_dunder(op, l, r)? {
                return Ok(b);
            }
        }
        Ok(match op {
            CmpOp::Eq => l.py_eq(r),
            CmpOp::NotEq => !l.py_eq(r),
            CmpOp::Is => values_identical(l, r),
            CmpOp::IsNot => !values_identical(l, r),
            CmpOp::Lt => match l.py_cmp(r) {
                Some(Less) => true,
                Some(_) => false,
                None => {
                    return Err(type_error(format!(
                        "'<' not supported between '{}' and '{}'",
                        l.type_name(),
                        r.type_name()
                    )))
                }
            },
            CmpOp::LtE => match l.py_cmp(r) {
                Some(Less | Equal) => true,
                Some(_) => false,
                None => return Err(type_error("comparison not supported")),
            },
            CmpOp::Gt => match l.py_cmp(r) {
                Some(Greater) => true,
                Some(_) => false,
                None => return Err(type_error("comparison not supported")),
            },
            CmpOp::GtE => match l.py_cmp(r) {
                Some(Greater | Equal) => true,
                Some(_) => false,
                None => return Err(type_error("comparison not supported")),
            },
            CmpOp::In => self.contains(r, l)?,
            CmpOp::NotIn => !self.contains(r, l)?,
        })
    }

    fn contains(&mut self, container: &Value, item: &Value) -> Result<bool, Unwind> {
        match container {
            Value::List(l) => {
                let items: Vec<Value> = l.borrow().clone();
                for v in &items {
                    if self.cmp_op(CmpOp::Eq, v, item)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Value::Tuple(t) => {
                let items = t.clone();
                for v in items.iter() {
                    if self.cmp_op(CmpOp::Eq, v, item)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Value::Str(s) => {
                let needle = item.py_str();
                Ok(s.contains(&needle))
            }
            Value::Dict(d) => {
                let key = item.to_hash_key()?;
                Ok(d.borrow().contains_key(&key))
            }
            Value::Set(s) => {
                let key = item.to_hash_key()?;
                // The synthetic `__typhon_frozen__` sentinel must not
                // be observable via `x in s` — a literal `"…frozen…"`
                // probe would otherwise return True on every
                // `freeze let`-marked set.
                if matches!(&key, HashKey::Str(name) if name.as_str() == "__typhon_frozen__") {
                    return Ok(false);
                }
                Ok(s.borrow().contains(&key))
            }
            Value::Range { start, stop, step } => match item {
                Value::Int(i) => {
                    let Some(i_small) = i.to_i64() else {
                        return Ok(false);
                    };
                    if *step > 0 {
                        Ok(i_small >= *start
                            && i_small < *stop
                            && (i_small - start).rem_euclid(*step) == 0)
                    } else if *step < 0 {
                        Ok(i_small <= *start
                            && i_small > *stop
                            && (start - i_small).rem_euclid(-*step) == 0)
                    } else {
                        Ok(false)
                    }
                }
                _ => Ok(false),
            },
            Value::Instance(i) => {
                // `x in obj` → obj.__contains__(x).
                if let Some(m) = self.find_method(&i.class, "__contains__") {
                    let res = self.call_value(
                        Value::BoundMethod {
                            receiver: Box::new(container.clone()),
                            function: m,
                        },
                        vec![item.clone()],
                        &[],
                    )?;
                    return Ok(res.truthy());
                }
                Err(type_error(format!(
                    "argument of type '{}' is not iterable",
                    container.type_name()
                )))
            }
            other => Err(type_error(format!(
                "argument of type '{}' is not iterable",
                other.type_name()
            ))),
        }
    }

    fn eval_call(&mut self, c: &ast::ExprCall, env: &EnvRef) -> Result<Value, Unwind> {
        let func = self.eval_expr(&c.func, env)?;
        let mut args = Vec::with_capacity(c.arguments.args.len());
        for arg in c.arguments.args.iter() {
            if let Expr::Starred(s) = arg {
                let v = self.eval_expr(&s.value, env)?;
                let iter = self.make_iter(v)?;
                while let Some(x) = self.iter_next(&iter)? {
                    args.push(x);
                }
            } else {
                args.push(self.eval_expr(arg, env)?);
            }
        }
        let mut kwargs: Vec<(String, Value)> = Vec::with_capacity(c.arguments.keywords.len());
        for kw in c.arguments.keywords.iter() {
            match &kw.arg {
                Some(name) => {
                    kwargs.push((name.as_str().to_owned(), self.eval_expr(&kw.value, env)?));
                }
                None => {
                    // **kwargs spread
                    let v = self.eval_expr(&kw.value, env)?;
                    match v {
                        Value::Dict(d) => {
                            for (k, val) in d.borrow().iter() {
                                if let HashKey::Str(s) = k {
                                    kwargs.push(((**s).clone(), val.clone()));
                                } else {
                                    return Err(type_error("keywords must be strings"));
                                }
                            }
                        }
                        _ => return Err(type_error("** argument must be a mapping")),
                    }
                }
            }
        }
        self.call_value(func, args, &kwargs)
    }

    pub fn call_value(
        &mut self,
        func: Value,
        args: Vec<Value>,
        kwargs: &[(String, Value)],
    ) -> Result<Value, Unwind> {
        match func {
            Value::Native(n) => {
                if !kwargs.is_empty() {
                    // For v1, native fns receive positional args only.
                    // Special-case common kwarg-accepting builtins (sorted, dict.get) by name.
                    return crate::builtins::call_with_kwargs(self, &n, args, kwargs);
                }
                (n.func)(self, args)
            }
            Value::Function(f) => self.call_function(&f, args, kwargs, None),
            Value::BoundMethod { receiver, function } => {
                let mut full_args = Vec::with_capacity(args.len() + 1);
                full_args.push(*receiver);
                full_args.extend(args);
                self.call_function(&function, full_args, kwargs, None)
            }
            Value::Class(c) => self.instantiate(&c, args, kwargs),
            other => Err(type_error(format!(
                "'{}' object is not callable",
                other.type_name()
            ))),
        }
    }

    fn call_function(
        &mut self,
        f: &Function,
        args: Vec<Value>,
        kwargs: &[(String, Value)],
        receiver: Option<Value>,
    ) -> Result<Value, Unwind> {
        if self.stack_depth >= self.max_stack_depth {
            return Err(Unwind::Exception(VmException::new(
                "RecursionError",
                "maximum recursion depth exceeded",
            )));
        }
        self.stack_depth += 1;
        // Wrap the body in a closure so every early `return` decrements the
        // counter on the way out — including a failure in `bind_args`.
        let call_env = Env::new_child(&f.closure);
        let result = (|| -> Result<Value, Unwind> {
            self.bind_args(f, args, kwargs, receiver, &call_env)?;
            match self.exec_block(&f.body, &call_env) {
                Ok(()) => Ok(Value::None),
                Err(Unwind::Return(v)) => Ok(v),
                Err(other) => Err(other),
            }
        })();
        self.stack_depth -= 1;
        result
    }

    fn bind_args(
        &mut self,
        f: &Function,
        args: Vec<Value>,
        kwargs: &[(String, Value)],
        receiver: Option<Value>,
        env: &EnvRef,
    ) -> Result<(), Unwind> {
        let params = &f.params;
        let positional: Vec<&ast::ParameterWithDefault> = params
            .posonlyargs
            .iter()
            .chain(params.args.iter())
            .collect();

        // Build effective positional list — `receiver` is prepended for bound methods.
        let mut all_pos: Vec<Value> = Vec::with_capacity(args.len() + receiver.is_some() as usize);
        if let Some(r) = receiver {
            all_pos.push(r);
        }
        all_pos.extend(args);

        // Track which kwargs have been consumed.
        let mut kwargs_left: HashMap<String, Value> =
            kwargs.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

        // `f.defaults` was evaluated once at def-time and is indexed by
        // `iter_non_variadic_params`: posonly, then args, then kwonlyargs.
        // We consume it in lockstep so each parameter's default fires
        // exactly once across all calls (matching Python's "default value
        // computed at def time" semantics, including the classic mutable-
        // default gotcha).
        let mut pos_iter = all_pos.into_iter();
        let mut defaults_iter = f.defaults.iter();
        for p in &positional {
            let name = p.parameter.name.as_str();
            let default = defaults_iter.next().and_then(|d| d.clone());
            if let Some(v) = pos_iter.next() {
                env.set(name, v);
            } else if let Some(v) = kwargs_left.remove(name) {
                env.set(name, v);
            } else if let Some(v) = default {
                env.set(name, v);
            } else {
                return Err(type_error(format!(
                    "{}() missing required argument: '{}'",
                    f.name, name
                )));
            }
        }

        // Remaining positionals → *args, else error.
        let remaining: Vec<Value> = pos_iter.collect();
        if let Some(va) = &params.vararg {
            env.set(va.name.as_str(), Value::Tuple(Rc::new(remaining)));
        } else if !remaining.is_empty() {
            return Err(type_error(format!(
                "{}() takes {} positional arguments but {} were given",
                f.name,
                positional.len(),
                positional.len() + remaining.len()
            )));
        }

        // Keyword-only params — defaults continue from the same iterator
        // since `iter_non_variadic_params` puts kwonlyargs after args.
        for p in &params.kwonlyargs {
            let name = p.parameter.name.as_str();
            let default = defaults_iter.next().and_then(|d| d.clone());
            if let Some(v) = kwargs_left.remove(name) {
                env.set(name, v);
            } else if let Some(v) = default {
                env.set(name, v);
            } else {
                return Err(type_error(format!(
                    "{}() missing required keyword-only argument: '{}'",
                    f.name, name
                )));
            }
        }

        if let Some(kw) = &params.kwarg {
            let mut map: DictMap = IndexMap::new();
            for (k, v) in kwargs_left.drain() {
                map.insert(HashKey::Str(Rc::new(k)), v);
            }
            env.set(kw.name.as_str(), Value::Dict(Rc::new(RefCell::new(map))));
        } else if !kwargs_left.is_empty() {
            let names: Vec<String> = kwargs_left.keys().cloned().collect();
            return Err(type_error(format!(
                "{}() got unexpected keyword argument(s): {}",
                f.name,
                names.join(", ")
            )));
        }
        Ok(())
    }

    fn instantiate(
        &mut self,
        class: &Rc<Class>,
        args: Vec<Value>,
        kwargs: &[(String, Value)],
    ) -> Result<Value, Unwind> {
        let instance = Rc::new(Instance {
            class: class.clone(),
            fields: RefCell::new(HashMap::new()),
        });
        // Initialise class-level attributes that aren't methods.
        for (k, v) in class.class_attrs.borrow().iter() {
            instance.fields.borrow_mut().insert(k.clone(), v.clone());
        }
        // Custom __init__ wins.
        if let Some(init) = self.find_method(class, "__init__") {
            self.call_function(&init, args, kwargs, Some(Value::Instance(instance.clone())))?;
            return Ok(Value::Instance(instance));
        }
        // Auto-generated __init__ from class fields (Typhon's dataclass default).
        let mut consumed_kwargs: HashMap<String, Value> =
            kwargs.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let mut pos = args.into_iter();
        for field in &class.fields {
            let val = if let Some(v) = pos.next() {
                v
            } else if let Some(v) = consumed_kwargs.remove(&field.name) {
                v
            } else if let Some(d) = &field.default {
                // Detect the `dataclasses.field(default_factory=...)`
                // sentinel emitted by the mutable-default rewrite. Each
                // instance gets a freshly-invoked factory result so
                // `tags: list[str] = []` doesn't share one list across
                // every instance.
                if let Value::Tuple(items) = d {
                    if items.len() == 2 {
                        if let Value::Str(tag) = &items[0] {
                            if tag.as_str() == "__typhon_field_factory__" {
                                let factory = items[1].clone();
                                self.call_value(factory, vec![], &[])?
                            } else {
                                d.clone()
                            }
                        } else {
                            d.clone()
                        }
                    } else {
                        d.clone()
                    }
                } else {
                    d.clone()
                }
            } else {
                return Err(type_error(format!(
                    "{}() missing required argument: '{}'",
                    class.name, field.name
                )));
            };
            instance.fields.borrow_mut().insert(field.name.clone(), val);
        }
        if pos.next().is_some() {
            return Err(type_error(format!(
                "{}() takes {} arguments (more given)",
                class.name,
                class.fields.len()
            )));
        }
        if !consumed_kwargs.is_empty() {
            let extras: Vec<String> = consumed_kwargs.keys().cloned().collect();
            return Err(type_error(format!(
                "{}() got unexpected keyword argument(s): {}",
                class.name,
                extras.join(", ")
            )));
        }
        Ok(Value::Instance(instance))
    }

    pub fn find_method(&self, class: &Rc<Class>, name: &str) -> Option<Rc<Function>> {
        if let Some(m) = class.methods.borrow().get(name) {
            return Some(m.clone());
        }
        for base in &class.bases {
            if let Some(m) = self.find_method(base, name) {
                return Some(m);
            }
        }
        None
    }

    /// Invoke a zero-argument dunder method on an instance, if defined.
    pub fn call_dunder0(&mut self, v: &Value, name: &str) -> Result<Option<Value>, Unwind> {
        if let Value::Instance(i) = v {
            if let Some(m) = self.find_method(&i.class, name) {
                let r = self.call_value(
                    Value::BoundMethod {
                        receiver: Box::new(v.clone()),
                        function: m,
                    },
                    vec![],
                    &[],
                )?;
                return Ok(Some(r));
            }
        }
        Ok(None)
    }

    /// `str(v)` honouring a user `__str__` (then `__repr__`) on instances.
    pub fn str_of(&mut self, v: &Value) -> Result<String, Unwind> {
        if let Some(r) = self.call_dunder0(v, "__str__")? {
            return Ok(r.py_str());
        }
        if let Some(r) = self.call_dunder0(v, "__repr__")? {
            return Ok(r.py_str());
        }
        Ok(v.py_str())
    }

    /// `repr(v)` honouring a user `__repr__` on instances.
    pub fn repr_of(&mut self, v: &Value) -> Result<String, Unwind> {
        if let Some(r) = self.call_dunder0(v, "__repr__")? {
            return Ok(r.py_str());
        }
        Ok(v.py_repr())
    }

    // ── Operators ──────────────────────────────────────────────────────────

    pub fn binop(&mut self, l: &Value, op: Operator, r: &Value) -> Result<Value, Unwind> {
        use Operator::*;
        use Value::*;
        // Fast-path numeric combinations. Integer ops now use `BigInt`
        // (FINDINGS #19) so `2 ** 100`, `fib(99)`, and friends no longer
        // overflow — matching Python's arbitrary-precision semantics.
        match (l, op, r) {
            (Int(a), Add, Int(b)) => return Ok(Int(a + b)),
            (Int(a), Sub, Int(b)) => return Ok(Int(a - b)),
            (Int(a), Mult, Int(b)) => return Ok(Int(a * b)),
            (Int(_), Div, Int(b)) if b.is_zero() => return Err(zero_division()),
            (Int(a), Div, Int(b)) => return Ok(Float(bigint_to_f64(a) / bigint_to_f64(b))),
            (Int(_), FloorDiv, Int(b)) if b.is_zero() => return Err(zero_division()),
            (Int(a), FloorDiv, Int(b)) => return Ok(Int(a.div_floor(b))),
            (Int(_), Mod, Int(b)) if b.is_zero() => return Err(zero_division()),
            (Int(a), Mod, Int(b)) => return Ok(Int(a.mod_floor(b))),
            (Int(a), Pow, Int(b)) => {
                if b.is_negative() {
                    return Ok(Float(bigint_to_f64(a).powf(bigint_to_f64(b))));
                }
                // BigInt::pow takes a `u32`; for ridiculous exponents
                // (10**million) we'd happily eat all the RAM, so cap at
                // u32::MAX which is already astronomically more than
                // Python tolerates before timing out.
                let exp = b.to_u32().ok_or_else(overflow)?;
                return Ok(Int(a.pow(exp)));
            }
            (Int(a), BitOr, Int(b)) => return Ok(Int(a | b)),
            (Int(a), BitAnd, Int(b)) => return Ok(Int(a & b)),
            (Int(a), BitXor, Int(b)) => return Ok(Int(a ^ b)),
            (Int(a), LShift, Int(b)) => {
                if b.is_negative() {
                    return Err(value_error("negative shift count"));
                }
                let shift = b.to_usize().ok_or_else(overflow)?;
                return Ok(Int(a << shift));
            }
            (Int(a), RShift, Int(b)) => {
                if b.is_negative() {
                    return Err(value_error("negative shift count"));
                }
                let shift = b.to_usize().unwrap_or(usize::MAX);
                return Ok(Int(a >> shift));
            }

            (Float(a), Add, Float(b)) => return Ok(Float(a + b)),
            (Float(a), Sub, Float(b)) => return Ok(Float(a - b)),
            (Float(a), Mult, Float(b)) => return Ok(Float(a * b)),
            (Float(a), Div, Float(b)) => {
                if *b == 0.0 {
                    return Err(zero_division());
                }
                return Ok(Float(a / b));
            }
            (Float(a), FloorDiv, Float(b)) => return Ok(Float((a / b).floor())),
            (Float(a), Mod, Float(b)) => return Ok(Float(a.rem_euclid(*b))),
            (Float(a), Pow, Float(b)) => return Ok(Float(a.powf(*b))),
            _ => {}
        }

        // Mixed int/float — promote to float.
        if matches!(
            (l, r),
            (Int(_) | Bool(_), Float(_)) | (Float(_), Int(_) | Bool(_))
        ) {
            let a = l.to_float()?;
            let b = r.to_float()?;
            return self.binop(&Float(a), op, &Float(b));
        }
        // Bool ↔ Int.
        if matches!((l, r), (Bool(_), Int(_) | Bool(_)) | (Int(_), Bool(_))) {
            let a = l.to_bigint()?;
            let b = r.to_bigint()?;
            return self.binop(&Int(a), op, &Int(b));
        }

        // Strings.
        if let (Str(a), Add, Str(b)) = (l, op, r) {
            return Ok(Str(Rc::new(format!("{}{}", a, b))));
        }
        if let (Str(a), Mult, Int(n)) = (l, op, r) {
            let n = n.to_usize().unwrap_or(0);
            return Ok(Str(Rc::new(a.repeat(n))));
        }
        if let (Int(n), Mult, Str(a)) = (l, op, r) {
            let n = n.to_usize().unwrap_or(0);
            return Ok(Str(Rc::new(a.repeat(n))));
        }

        // Lists / tuples.
        if let (List(a), Add, List(b)) = (l, op, r) {
            let mut out = a.borrow().clone();
            out.extend(b.borrow().iter().cloned());
            return Ok(List(Rc::new(RefCell::new(out))));
        }
        if let (Tuple(a), Add, Tuple(b)) = (l, op, r) {
            let mut out = a.as_ref().clone();
            out.extend(b.iter().cloned());
            return Ok(Tuple(Rc::new(out)));
        }
        if let (List(a), Mult, Int(n)) = (l, op, r) {
            let n = n.to_usize().unwrap_or(0);
            let mut out = Vec::with_capacity(a.borrow().len() * n);
            for _ in 0..n {
                out.extend(a.borrow().iter().cloned());
            }
            return Ok(List(Rc::new(RefCell::new(out))));
        }
        if let (Tuple(a), Mult, Int(n)) = (l, op, r) {
            let n = n.to_usize().unwrap_or(0);
            let mut out = Vec::with_capacity(a.len() * n);
            for _ in 0..n {
                out.extend(a.iter().cloned());
            }
            return Ok(Tuple(Rc::new(out)));
        }

        // Sets.
        if let (Set(a), op2, Set(b)) = (l, op, r) {
            let a = a.borrow();
            let b = b.borrow();
            let out: std::collections::HashSet<HashKey> = match op2 {
                BitOr => a.union(&b).cloned().collect(),
                BitAnd => a.intersection(&b).cloned().collect(),
                Sub => a.difference(&b).cloned().collect(),
                BitXor => a.symmetric_difference(&b).cloned().collect(),
                _ => return Err(type_error("unsupported set operation")),
            };
            return Ok(Set(Rc::new(RefCell::new(out))));
        }

        // Operator overloading: dispatch to the left operand's dunder method,
        // falling back to the right operand's reflected dunder (`__radd__`).
        if let Some(dunder) = binop_dunder(op) {
            if let Value::Instance(i) = l {
                if let Some(m) = self.find_method(&i.class, dunder) {
                    return self.call_value(
                        Value::BoundMethod {
                            receiver: Box::new(l.clone()),
                            function: m,
                        },
                        vec![r.clone()],
                        &[],
                    );
                }
            }
            if let (Some(rdunder), Value::Instance(i)) = (binop_reflected_dunder(op), r) {
                if let Some(m) = self.find_method(&i.class, rdunder) {
                    return self.call_value(
                        Value::BoundMethod {
                            receiver: Box::new(r.clone()),
                            function: m,
                        },
                        vec![l.clone()],
                        &[],
                    );
                }
            }
        }

        Err(type_error(format!(
            "unsupported operand type(s) for {}: '{}' and '{}'",
            op.as_str(),
            l.type_name(),
            r.type_name()
        )))
    }

    fn unop(&mut self, op: UnaryOp, v: &Value) -> Result<Value, Unwind> {
        match op {
            UnaryOp::Not => Ok(Value::Bool(!v.truthy())),
            UnaryOp::USub => match v {
                Value::Int(i) => Ok(Value::Int(-i)),
                Value::Float(x) => Ok(Value::Float(-*x)),
                Value::Bool(b) => Ok(Value::Int(BigInt::from(-(*b as i64)))),
                _ => Err(type_error(format!(
                    "bad operand for unary -: '{}'",
                    v.type_name()
                ))),
            },
            UnaryOp::UAdd => match v {
                Value::Int(_) | Value::Float(_) => Ok(v.clone()),
                Value::Bool(b) => Ok(Value::Int(BigInt::from(*b as i64))),
                _ => Err(type_error("bad operand for unary +")),
            },
            UnaryOp::Invert => match v {
                Value::Int(i) => Ok(Value::Int(!i)),
                Value::Bool(b) => Ok(Value::Int(!BigInt::from(*b as i64))),
                _ => Err(type_error("bad operand for unary ~")),
            },
        }
    }

    // ── Subscript / attribute / target assignment ──────────────────────────

    pub fn subscript(&mut self, target: &Value, key: &Value) -> Result<Value, Unwind> {
        // Slice marker?
        if let Value::Tuple(t) = key {
            if t.len() == 4 {
                if let Value::Str(tag) = &t[0] {
                    if tag.as_str() == "__slice__" {
                        return self.slice_target(target, &t[1], &t[2], &t[3]);
                    }
                }
            }
        }
        match target {
            Value::List(l) => {
                let i = key.to_int()?;
                let l = l.borrow();
                let idx = normalize_index(i, l.len())
                    .ok_or_else(|| index_error("list index out of range"))?;
                Ok(l[idx].clone())
            }
            Value::Tuple(t) => {
                let i = key.to_int()?;
                let idx = normalize_index(i, t.len())
                    .ok_or_else(|| index_error("tuple index out of range"))?;
                Ok(t[idx].clone())
            }
            Value::Str(s) => {
                let i = key.to_int()?;
                let chars: Vec<char> = s.chars().collect();
                let idx = normalize_index(i, chars.len())
                    .ok_or_else(|| index_error("string index out of range"))?;
                Ok(Value::Str(Rc::new(chars[idx].to_string())))
            }
            Value::Dict(d) => {
                let k = key.to_hash_key()?;
                d.borrow()
                    .get(&k)
                    .cloned()
                    .ok_or_else(|| key_error(key.py_repr()))
            }
            Value::Bytes(b) => {
                let i = key.to_int()?;
                let idx = normalize_index(i, b.len())
                    .ok_or_else(|| index_error("bytes index out of range"))?;
                Ok(Value::Int(BigInt::from(b[idx] as i64)))
            }
            Value::Instance(i) => {
                if let Some(m) = self.find_method(&i.class, "__getitem__") {
                    return self.call_value(
                        Value::BoundMethod {
                            receiver: Box::new(target.clone()),
                            function: m,
                        },
                        vec![key.clone()],
                        &[],
                    );
                }
                Err(type_error(format!(
                    "'{}' object is not subscriptable",
                    target.type_name()
                )))
            }
            other => Err(type_error(format!(
                "'{}' object is not subscriptable",
                other.type_name()
            ))),
        }
    }

    fn slice_target(
        &mut self,
        target: &Value,
        lower: &Value,
        upper: &Value,
        step: &Value,
    ) -> Result<Value, Unwind> {
        let len = match target {
            Value::List(l) => l.borrow().len(),
            Value::Tuple(t) => t.len(),
            Value::Str(s) => s.chars().count(),
            Value::Bytes(b) => b.len(),
            _ => {
                return Err(type_error(format!(
                    "'{}' object is not sliceable",
                    target.type_name()
                )))
            }
        };
        let step_i = match step {
            Value::None => 1,
            v => v.to_int()?,
        };
        if step_i == 0 {
            return Err(value_error("slice step cannot be zero"));
        }
        let (start, stop, step_i) = compute_slice(lower, upper, step_i, len)?;
        let mut indices: Vec<usize> = Vec::new();
        if step_i > 0 {
            let mut idx = start;
            while idx < stop {
                if idx >= 0 {
                    indices.push(idx as usize);
                }
                idx += step_i;
            }
        } else {
            let mut idx = start;
            while idx > stop {
                if idx >= 0 {
                    indices.push(idx as usize);
                }
                idx += step_i;
            }
        }
        match target {
            Value::List(l) => {
                let l = l.borrow();
                let v: Vec<Value> = indices
                    .into_iter()
                    .filter_map(|i| l.get(i).cloned())
                    .collect();
                Ok(Value::List(Rc::new(RefCell::new(v))))
            }
            Value::Tuple(t) => {
                let v: Vec<Value> = indices
                    .into_iter()
                    .filter_map(|i| t.get(i).cloned())
                    .collect();
                Ok(Value::Tuple(Rc::new(v)))
            }
            Value::Str(s) => {
                let chars: Vec<char> = s.chars().collect();
                let out: String = indices.into_iter().filter_map(|i| chars.get(i)).collect();
                Ok(Value::Str(Rc::new(out)))
            }
            Value::Bytes(b) => {
                let out: Vec<u8> = indices
                    .into_iter()
                    .filter_map(|i| b.get(i).copied())
                    .collect();
                Ok(Value::Bytes(Rc::new(out)))
            }
            _ => unreachable!(),
        }
    }

    fn del_subscript(&mut self, target: &Value, key: &Value) -> Result<(), Unwind> {
        match target {
            Value::List(l) => {
                let i = key.to_int()?;
                let mut l = l.borrow_mut();
                let idx = normalize_index(i, l.len())
                    .ok_or_else(|| index_error("list index out of range"))?;
                l.remove(idx);
                Ok(())
            }
            Value::Dict(d) => {
                let k = key.to_hash_key()?;
                // `shift_remove` preserves insertion order (matches Python's
                // `del d[k]` on an insertion-ordered dict).
                d.borrow_mut()
                    .shift_remove(&k)
                    .map(|_| ())
                    .ok_or_else(|| key_error(key.py_repr()))
            }
            _ => Err(type_error("delete on unsupported target")),
        }
    }

    pub fn get_attr(&mut self, value: &Value, attr: &str) -> Result<Value, Unwind> {
        // Try instance fields first, then class methods.
        match value {
            Value::Instance(inst) => {
                if let Some(v) = inst.fields.borrow().get(attr) {
                    return Ok(v.clone());
                }
                // `@property` getters are invoked on read, not returned as a method.
                if inst.class.properties.borrow().contains(attr) {
                    if let Some(m) = self.find_method(&inst.class, attr) {
                        return self.call_value(
                            Value::BoundMethod {
                                receiver: Box::new(value.clone()),
                                function: m,
                            },
                            vec![],
                            &[],
                        );
                    }
                }
                if let Some(m) = self.find_method(&inst.class, attr) {
                    // `@classmethod` binds the class object as `cls`, not the instance.
                    let receiver = if inst.class.classmethods.borrow().contains(attr) {
                        Box::new(Value::Class(inst.class.clone()))
                    } else {
                        Box::new(value.clone())
                    };
                    return Ok(Value::BoundMethod {
                        receiver,
                        function: m,
                    });
                }
                Err(attribute_error(format!(
                    "'{}' object has no attribute '{}'",
                    inst.class.name, attr
                )))
            }
            Value::Class(class) => {
                if let Some(v) = class.class_attrs.borrow().get(attr) {
                    return Ok(v.clone());
                }
                if let Some(m) = class.methods.borrow().get(attr) {
                    // `@classmethod` accessed on the class binds `cls` to the class.
                    if class.classmethods.borrow().contains(attr) {
                        return Ok(Value::BoundMethod {
                            receiver: Box::new(value.clone()),
                            function: m.clone(),
                        });
                    }
                    return Ok(Value::Function(m.clone()));
                }
                Err(attribute_error(format!(
                    "type object '{}' has no attribute '{}'",
                    class.name, attr
                )))
            }
            Value::Module(m) => m.members.borrow().get(attr).cloned().ok_or_else(|| {
                attribute_error(format!("module '{}' has no attribute '{}'", m.name, attr))
            }),
            Value::ResultOk(v) => match attr {
                "value" => Ok((**v).clone()),
                "map" | "map_err" | "and_then" | "or_else" => {
                    Ok(bind_result_combinator(value.clone(), attr))
                }
                _ => Err(attribute_error(format!("Ok has no attribute '{}'", attr))),
            },
            Value::ResultErr(v) => match attr {
                "value" | "error" => Ok((**v).clone()),
                "map" | "map_err" | "and_then" | "or_else" => {
                    Ok(bind_result_combinator(value.clone(), attr))
                }
                _ => Err(attribute_error(format!("Err has no attribute '{}'", attr))),
            },
            Value::Str(_)
            | Value::List(_)
            | Value::Dict(_)
            | Value::Set(_)
            | Value::Tuple(_)
            | Value::Int(_)
            | Value::Float(_)
            | Value::Bool(_)
            | Value::Bytes(_) => {
                // Return a native fn whose first arg is the receiver — the
                // method registry in `builtins` does the actual dispatch.
                let r = value.clone();
                let attr_name: Rc<str> = Rc::from(attr);
                let nf = NativeFn::new("method", move |interp, mut args| {
                    args.insert(0, r.clone());
                    crate::builtins::dispatch_method(interp, &attr_name, args)
                });
                Ok(Value::Native(Rc::new(nf)))
            }
            // Unbound builtin-type methods: `str.strip(x)`, `list.append(xs, v)`,
            // `dict.get(d, k)`. The type constructors are registered as natives
            // named after the type; accessing a method on one yields a function
            // that dispatches with the explicit receiver as its first argument.
            // This is what the documented pipe idiom `x |> str.lower()` lowers to.
            Value::Native(nf)
                if matches!(
                    nf.name,
                    "str" | "list" | "dict" | "set" | "frozenset" | "tuple" | "bytes"
                ) =>
            {
                let attr_name: Rc<str> = Rc::from(attr);
                let m = NativeFn::new("method", move |interp, args| {
                    if args.is_empty() {
                        return Err(type_error(format!(
                            "unbound method '{}' needs an argument",
                            attr_name
                        )));
                    }
                    crate::builtins::dispatch_method(interp, &attr_name, args)
                });
                Ok(Value::Native(Rc::new(m)))
            }
            Value::Exception { kind, message } => match attr {
                "args" => Ok(Value::Tuple(Rc::new(vec![Value::Str(message.clone())]))),
                "kind" => Ok(Value::Str(kind.clone())),
                _ => Err(attribute_error(format!(
                    "'{}' has no attribute '{}'",
                    kind, attr
                ))),
            },
            other => Err(attribute_error(format!(
                "'{}' object has no attribute '{}'",
                other.type_name(),
                attr
            ))),
        }
    }

    fn assign_target(
        &mut self,
        target: &Expr,
        value: Value,
        env: &EnvRef,
        _mutability: Option<Mutability>,
    ) -> Result<(), Unwind> {
        match target {
            Expr::Name(n) => {
                env.assign_or_create(n.id.as_str(), value);
                Ok(())
            }
            Expr::Tuple(t) => self.assign_unpack(&t.elts, value, env),
            Expr::List(l) => self.assign_unpack(&l.elts, value, env),
            Expr::Attribute(a) => {
                let recv = self.eval_expr(&a.value, env)?;
                self.set_attr(&recv, a.attr.as_str(), value)
            }
            Expr::Subscript(s) => {
                let target_v = self.eval_expr(&s.value, env)?;
                let key = self.eval_subscript_index(&s.slice, env)?;
                self.set_subscript(&target_v, &key, value)
            }
            Expr::Starred(s) => self.assign_target(&s.value, value, env, None),
            _ => Err(type_error("invalid assignment target")),
        }
    }

    fn assign_unpack(&mut self, elts: &[Expr], value: Value, env: &EnvRef) -> Result<(), Unwind> {
        let mut items: Vec<Value> = match value {
            Value::Tuple(t) => t.as_ref().clone(),
            Value::List(l) => l.borrow().clone(),
            other => {
                // Iterate.
                let iter = self.make_iter(other)?;
                let mut out = Vec::new();
                while let Some(v) = self.iter_next(&iter)? {
                    out.push(v);
                }
                out
            }
        };
        // Look for a starred target.
        let star_idx = elts.iter().position(|e| matches!(e, Expr::Starred(_)));
        if let Some(s) = star_idx {
            let before = s;
            let after = elts.len() - s - 1;
            if items.len() < before + after {
                return Err(value_error("not enough values to unpack"));
            }
            let mid_count = items.len() - before - after;
            let after_items: Vec<Value> = items.drain(items.len() - after..).collect();
            let mid: Vec<Value> = items.drain(before..before + mid_count).collect();
            // items now holds the `before` values.
            for (t, v) in elts[..before].iter().zip(items.into_iter()) {
                self.assign_target(t, v, env, None)?;
            }
            self.assign_target(&elts[s], Value::List(Rc::new(RefCell::new(mid))), env, None)?;
            for (t, v) in elts[s + 1..].iter().zip(after_items.into_iter()) {
                self.assign_target(t, v, env, None)?;
            }
            Ok(())
        } else {
            if items.len() != elts.len() {
                return Err(value_error(format!(
                    "expected {} values to unpack, got {}",
                    elts.len(),
                    items.len()
                )));
            }
            for (t, v) in elts.iter().zip(items.into_iter()) {
                self.assign_target(t, v, env, None)?;
            }
            Ok(())
        }
    }

    fn set_attr(&mut self, receiver: &Value, attr: &str, value: Value) -> Result<(), Unwind> {
        match receiver {
            Value::Instance(i) => {
                i.fields.borrow_mut().insert(attr.to_owned(), value);
                Ok(())
            }
            Value::Class(c) => {
                c.class_attrs.borrow_mut().insert(attr.to_owned(), value);
                Ok(())
            }
            Value::Module(m) => {
                m.members.borrow_mut().insert(attr.to_owned(), value);
                Ok(())
            }
            other => Err(type_error(format!(
                "cannot set attribute on '{}'",
                other.type_name()
            ))),
        }
    }

    fn set_subscript(&mut self, target: &Value, key: &Value, value: Value) -> Result<(), Unwind> {
        match target {
            Value::List(l) => {
                let i = key.to_int()?;
                let mut l = l.borrow_mut();
                let idx = normalize_index(i, l.len())
                    .ok_or_else(|| index_error("list index out of range"))?;
                l[idx] = value;
                Ok(())
            }
            Value::Dict(d) => {
                if crate::builtins::dict_is_frozen(d) {
                    return Err(type_error(
                        "'mappingproxy' object does not support item assignment",
                    ));
                }
                let k = key.to_hash_key()?;
                d.borrow_mut().insert(k, value);
                Ok(())
            }
            other => Err(type_error(format!(
                "'{}' object does not support item assignment",
                other.type_name()
            ))),
        }
    }

    // ── Iteration ──────────────────────────────────────────────────────────

    pub fn make_iter(&mut self, v: Value) -> Result<Value, Unwind> {
        let state = match v {
            Value::Range { start, stop, step } => IterState::Range {
                current: start,
                stop,
                step,
            },
            Value::List(l) => IterState::List { items: l, index: 0 },
            Value::Tuple(t) => IterState::Tuple { items: t, index: 0 },
            Value::Str(s) => {
                let chars = s.chars().collect();
                IterState::Str { chars, index: 0 }
            }
            Value::Dict(d) => {
                let keys: Vec<HashKey> = d
                    .borrow()
                    .keys()
                    .filter(|k| !matches!(k, HashKey::Str(s) if s.as_str() == "__typhon_frozen__"))
                    .cloned()
                    .collect();
                IterState::Dict { keys, index: 0 }
            }
            Value::Set(s) => {
                // Filter the synthetic `__typhon_frozen__` sentinel
                // `deep_freeze_value` inserts to mark the set
                // immutable.
                let keys: Vec<HashKey> = s
                    .borrow()
                    .iter()
                    .filter(|k| !matches!(k, HashKey::Str(s) if s.as_str() == "__typhon_frozen__"))
                    .cloned()
                    .collect();
                IterState::Set { keys, index: 0 }
            }
            Value::Iter(it) => return Ok(Value::Iter(it)),
            other => {
                return Err(type_error(format!(
                    "'{}' object is not iterable",
                    other.type_name()
                )))
            }
        };
        Ok(Value::Iter(Rc::new(RefCell::new(state))))
    }

    pub fn iter_next(&mut self, it: &Value) -> Result<Option<Value>, Unwind> {
        let Value::Iter(state) = it else {
            return Err(type_error("not an iterator"));
        };
        // Iterator adapters that wrap an inner iterator (Enumerate, Zip, Map,
        // Filter) must recurse into `iter_next` on that inner iterator. The
        // recursion needs `&mut self`, and bumping our own state afterwards
        // needs a fresh `state.borrow_mut()`, so we cannot keep the scrutinee
        // borrow alive across it. Handle the leaf states inline (returning from
        // within the borrow scope) and, for the recursive states, clone out the
        // handles we need and drop the borrow *before* recursing.
        enum Recurse {
            Enumerate(Rc<RefCell<IterState>>),
            Zip(Vec<Rc<RefCell<IterState>>>),
            Map(Value, Rc<RefCell<IterState>>),
            Filter(Value, Rc<RefCell<IterState>>),
        }

        let recurse = {
            let mut guard = state.borrow_mut();
            match &mut *guard {
                IterState::Range {
                    current,
                    stop,
                    step,
                } => {
                    let done = if *step > 0 {
                        *current >= *stop
                    } else {
                        *current <= *stop
                    };
                    return if done {
                        Ok(None)
                    } else {
                        let v = Value::Int(BigInt::from(*current));
                        *current += *step;
                        Ok(Some(v))
                    };
                }
                IterState::List { items, index } => {
                    let l = items.borrow();
                    return if *index >= l.len() {
                        Ok(None)
                    } else {
                        let v = l[*index].clone();
                        *index += 1;
                        Ok(Some(v))
                    };
                }
                IterState::Tuple { items, index } => {
                    return if *index >= items.len() {
                        Ok(None)
                    } else {
                        let v = items[*index].clone();
                        *index += 1;
                        Ok(Some(v))
                    };
                }
                IterState::Str { chars, index } => {
                    return if *index >= chars.len() {
                        Ok(None)
                    } else {
                        let v = Value::Str(Rc::new(chars[*index].to_string()));
                        *index += 1;
                        Ok(Some(v))
                    };
                }
                IterState::Dict { keys, index } => {
                    return if *index >= keys.len() {
                        Ok(None)
                    } else {
                        let v = keys[*index].clone().into_value();
                        *index += 1;
                        Ok(Some(v))
                    };
                }
                IterState::Set { keys, index } => {
                    return if *index >= keys.len() {
                        Ok(None)
                    } else {
                        let v = keys[*index].clone().into_value();
                        *index += 1;
                        Ok(Some(v))
                    };
                }
                IterState::Enumerate { inner, .. } => Recurse::Enumerate(inner.clone()),
                IterState::Zip { inners } => Recurse::Zip(inners.clone()),
                IterState::Map { func, inner } => Recurse::Map(func.clone(), inner.clone()),
                IterState::Filter { func, inner } => Recurse::Filter(func.clone(), inner.clone()),
            }
        };

        match recurse {
            Recurse::Enumerate(inner) => match self.iter_next(&Value::Iter(inner))? {
                Some(v) => {
                    let idx = match &mut *state.borrow_mut() {
                        IterState::Enumerate { index, .. } => {
                            let i = *index;
                            *index += 1;
                            i
                        }
                        _ => unreachable!(),
                    };
                    Ok(Some(Value::Tuple(Rc::new(vec![
                        Value::Int(BigInt::from(idx)),
                        v,
                    ]))))
                }
                None => Ok(None),
            },
            Recurse::Zip(inners) => {
                let mut out = Vec::with_capacity(inners.len());
                for i in &inners {
                    match self.iter_next(&Value::Iter(i.clone()))? {
                        Some(v) => out.push(v),
                        None => return Ok(None),
                    }
                }
                Ok(Some(Value::Tuple(Rc::new(out))))
            }
            Recurse::Map(func, inner) => match self.iter_next(&Value::Iter(inner))? {
                Some(v) => Ok(Some(self.call_value(func, vec![v], &[])?)),
                None => Ok(None),
            },
            Recurse::Filter(func, inner) => loop {
                match self.iter_next(&Value::Iter(inner.clone()))? {
                    Some(v) => {
                        let keep = self
                            .call_value(func.clone(), vec![v.clone()], &[])?
                            .truthy();
                        if keep {
                            return Ok(Some(v));
                        }
                    }
                    None => return Ok(None),
                }
            },
        }
    }

    // ── Comprehensions ─────────────────────────────────────────────────────

    fn run_comprehension<F>(
        &mut self,
        generators: &[ast::Comprehension],
        env: &EnvRef,
        mut emit: F,
    ) -> Result<(), Unwind>
    where
        F: FnMut(&mut Self, &EnvRef) -> Result<(), Unwind>,
    {
        let scope = Env::new_child(env);
        self.run_comp_recurse(generators, 0, &scope, &mut emit)
    }

    fn run_comp_recurse<F>(
        &mut self,
        gens: &[ast::Comprehension],
        i: usize,
        env: &EnvRef,
        emit: &mut F,
    ) -> Result<(), Unwind>
    where
        F: FnMut(&mut Self, &EnvRef) -> Result<(), Unwind>,
    {
        if i == gens.len() {
            return emit(self, env);
        }
        let g = &gens[i];
        let iter_val = self.eval_expr(&g.iter, env)?;
        let it = self.make_iter(iter_val)?;
        while let Some(v) = self.iter_next(&it)? {
            self.assign_target(&g.target, v, env, None)?;
            let mut ok = true;
            for cond in &g.ifs {
                if !self.eval_expr(cond, env)?.truthy() {
                    ok = false;
                    break;
                }
            }
            if ok {
                self.run_comp_recurse(gens, i + 1, env, emit)?;
            }
        }
        Ok(())
    }

    fn eval_listcomp(&mut self, c: &ast::ExprListComp, env: &EnvRef) -> Result<Value, Unwind> {
        let out = Rc::new(RefCell::new(Vec::new()));
        let elt = c.elt.clone();
        let out_clone = out.clone();
        self.run_comprehension(&c.generators, env, move |this, scope| {
            let v = this.eval_expr(&elt, scope)?;
            out_clone.borrow_mut().push(v);
            Ok(())
        })?;
        let result = std::mem::take(&mut *out.borrow_mut());
        Ok(Value::List(Rc::new(RefCell::new(result))))
    }

    fn eval_setcomp(&mut self, c: &ast::ExprSetComp, env: &EnvRef) -> Result<Value, Unwind> {
        let out = Rc::new(RefCell::new(std::collections::HashSet::new()));
        let elt = c.elt.clone();
        let out_clone = out.clone();
        self.run_comprehension(&c.generators, env, move |this, scope| {
            let v = this.eval_expr(&elt, scope)?;
            out_clone.borrow_mut().insert(v.to_hash_key()?);
            Ok(())
        })?;
        let result = std::mem::take(&mut *out.borrow_mut());
        Ok(Value::Set(Rc::new(RefCell::new(result))))
    }

    fn eval_dictcomp(&mut self, c: &ast::ExprDictComp, env: &EnvRef) -> Result<Value, Unwind> {
        let out: Rc<RefCell<DictMap>> = Rc::new(RefCell::new(IndexMap::new()));
        let key_expr = c
            .key
            .clone()
            .ok_or_else(|| type_error("dict comprehension missing key"))?;
        let value_expr = c.value.clone();
        let out_clone = out.clone();
        self.run_comprehension(&c.generators, env, move |this, scope| {
            let k = this.eval_expr(&key_expr, scope)?.to_hash_key()?;
            let v = this.eval_expr(&value_expr, scope)?;
            out_clone.borrow_mut().insert(k, v);
            Ok(())
        })?;
        let result = std::mem::take(&mut *out.borrow_mut());
        Ok(Value::Dict(Rc::new(RefCell::new(result))))
    }

    // ── try/except ─────────────────────────────────────────────────────────

    fn exec_try(&mut self, t: &ast::StmtTry, env: &EnvRef) -> Result<(), Unwind> {
        let body_res = self.exec_block(&t.body, env);
        let mut handled_exc: Option<Value> = None;
        let body_res = match body_res {
            Ok(()) => {
                // Run `else` block.
                self.exec_block(&t.orelse, env)
            }
            Err(Unwind::Exception(exc)) => {
                let mut found = false;
                for handler in &t.handlers {
                    let ExceptHandler::ExceptHandler(h) = handler;
                    let matches = match &h.type_ {
                        None => true,
                        Some(type_expr) => self.exception_matches(type_expr, &exc, env)?,
                    };
                    if matches {
                        found = true;
                        // If the raised exception carried a user-constructed
                        // Instance (typical for `raise HttpError(code=500,
                        // message="boom")` against a `class!` declaration),
                        // bind THAT instance to the handler name so that
                        // `e.code` / `e.message` work. Otherwise fall back
                        // to the bare `Value::Exception` summary.
                        let value = match &exc.value {
                            Some(v @ Value::Instance(_)) => v.clone(),
                            _ => Value::Exception {
                                kind: Rc::new(exc.kind.clone()),
                                message: Rc::new(exc.message.clone()),
                            },
                        };
                        if let Some(name) = &h.name {
                            env.set(name.as_str(), value.clone());
                        }
                        handled_exc = Some(value);
                        let result = self.exec_block(&h.body, env);
                        if let Some(name) = &h.name {
                            env.delete(name.as_str());
                        }
                        if let Err(e) = result {
                            // finally still runs.
                            let _ = self.exec_block(&t.finalbody, env);
                            return Err(e);
                        }
                        break;
                    }
                }
                if !found {
                    let _ = self.exec_block(&t.finalbody, env);
                    return Err(Unwind::Exception(exc));
                }
                Ok(())
            }
            Err(other) => {
                let _ = self.exec_block(&t.finalbody, env);
                return Err(other);
            }
        };
        let _ = handled_exc;
        self.exec_block(&t.finalbody, env)?;
        body_res
    }

    fn exception_matches(
        &mut self,
        type_expr: &Expr,
        exc: &VmException,
        env: &EnvRef,
    ) -> Result<bool, Unwind> {
        // Allow tuple of types.
        if let Expr::Tuple(t) = type_expr {
            for e in &t.elts {
                if self.exception_matches(e, exc, env)? {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        // Try to resolve to a name (e.g. `ValueError`); if it isn't bound,
        // accept any name match against the exception's `kind`.
        if let Expr::Name(n) = type_expr {
            let name = n.id.as_str();
            // Direct name match against the exception's kind.
            if name == exc.kind || name == "Exception" || name == "BaseException" {
                return Ok(true);
            }
            // Class-hierarchy match: if the exception carries a user
            // Instance, walk its MRO against the named class.
            if let Some(Value::Instance(inst)) = &exc.value {
                if let Ok(Value::Class(target)) = self.eval_expr(type_expr, env) {
                    if class_is_subclass(&inst.class, &target) {
                        return Ok(true);
                    }
                }
            }
            return Ok(false);
        }
        // Bare `except:` already returned true.
        let _ = self.eval_expr(type_expr, env);
        Ok(false)
    }

    fn value_to_exception(&self, v: Value) -> Unwind {
        match v {
            Value::Exception { kind, message } => {
                Unwind::Exception(VmException::new((*kind).clone(), (*message).clone()))
            }
            Value::Instance(i) => {
                // Prefer `args` (Python convention), fall back to `message`,
                // else stringify nothing. Never panic — this is the error
                // path we hit during a `raise`.
                let msg = {
                    let fields = i.fields.borrow();
                    fields
                        .get("args")
                        .or_else(|| fields.get("message"))
                        .map(|v| v.py_str())
                        .unwrap_or_default()
                };
                Unwind::Exception(
                    VmException::new(i.class.name.clone(), msg).with_value(Value::Instance(i)),
                )
            }
            other => {
                Unwind::Exception(VmException::new("Exception", other.py_str()).with_value(other))
            }
        }
    }

    // ── with ───────────────────────────────────────────────────────────────

    fn exec_with(&mut self, w: &ast::StmtWith, env: &EnvRef) -> Result<(), Unwind> {
        if w.is_async {
            return Err(vm_unsupported_use_compile("async with"));
        }
        // Each context-manager value must support .__enter__ / .__exit__.
        // For v1 we only handle plain values that implement these as native
        // methods (e.g. the file handle from `open()`).
        let mut entered: Vec<Value> = Vec::with_capacity(w.items.len());
        for item in &w.items {
            let cm = self.eval_expr(&item.context_expr, env)?;
            // Strict context-manager protocol: __enter__ must exist. The
            // file shim in `ffi.rs` returns the file object itself from
            // __enter__, so a `with open(...) as f:` block binds `f` to
            // the file.
            let enter = self.get_attr(&cm, "__enter__")?;
            let val = self.call_value(enter, vec![], &[])?;
            if let Some(t) = &item.optional_vars {
                self.assign_target(t, val, env, None)?;
            }
            entered.push(cm);
        }
        let body_res = self.exec_block(&w.body, env);
        // Call __exit__ on each, in reverse order.
        for cm in entered.into_iter().rev() {
            if let Ok(exit) = self.get_attr(&cm, "__exit__") {
                let _ = self.call_value(exit, vec![Value::None, Value::None, Value::None], &[]);
            }
        }
        body_res
    }

    // ── match ──────────────────────────────────────────────────────────────

    fn exec_match(&mut self, m: &ast::StmtMatch, env: &EnvRef) -> Result<(), Unwind> {
        let subject = self.eval_expr(&m.subject, env)?;
        for case in &m.cases {
            // Pattern captures bind tentatively into a child scope so a
            // *failed* pattern can't leak partial captures into `env`.
            // Once the pattern (and its guard) accept, we lift those
            // captures into the surrounding scope and execute the body
            // there — Python's `match` doesn't introduce a new scope per
            // arm, so writes inside the body must reach the outer
            // function's bindings.
            let scope = Env::new_child(env);
            if self.pattern_matches(&case.pattern, &subject, &scope)? {
                let ok = match &case.guard {
                    Some(g) => self.eval_expr(g, &scope)?.truthy(),
                    None => true,
                };
                if ok {
                    for (name, value) in scope.snapshot() {
                        env.assign_or_create(&name, value);
                    }
                    return self.exec_block(&case.body, env);
                }
            }
        }
        Ok(())
    }

    fn pattern_matches(
        &mut self,
        pat: &Pattern,
        subject: &Value,
        env: &EnvRef,
    ) -> Result<bool, Unwind> {
        use Pattern::*;
        match pat {
            MatchValue(v) => {
                let target = self.eval_expr(&v.value, env)?;
                Ok(subject.py_eq(&target))
            }
            MatchSingleton(s) => Ok(matches!(
                (&s.value, subject),
                (ast::Singleton::None, Value::None)
                    | (ast::Singleton::True, Value::Bool(true))
                    | (ast::Singleton::False, Value::Bool(false))
            )),
            MatchAs(a) => {
                let inner_ok = match &a.pattern {
                    Some(inner) => self.pattern_matches(inner, subject, env)?,
                    None => true,
                };
                if inner_ok {
                    if let Some(name) = &a.name {
                        env.set(name.as_str(), subject.clone());
                    }
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            MatchOr(o) => {
                for p in &o.patterns {
                    if self.pattern_matches(p, subject, env)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            MatchSequence(s) => match subject {
                Value::List(l) => self.pattern_match_seq(&s.patterns, &l.borrow(), env),
                Value::Tuple(t) => self.pattern_match_seq(&s.patterns, t, env),
                _ => Ok(false),
            },
            MatchClass(c) => self.pattern_match_class(c, subject, env),
            MatchMapping(m) => self.pattern_match_mapping(m, subject, env),
            // `MatchStar` only appears *inside* a sequence pattern (we
            // dispatch it there); seeing it here means a star pattern at
            // top level, which Python rejects with a SyntaxError. Treat
            // as a non-match rather than crashing.
            MatchStar(_) => Ok(false),
        }
    }

    /// FINDINGS #30 — mapping pattern (`case {"k": v, **rest}`). Matches
    /// against a dict subject, binds nested patterns for each named key,
    /// and (if present) captures the remaining keys into `rest`.
    fn pattern_match_mapping(
        &mut self,
        m: &ast::PatternMatchMapping,
        subject: &Value,
        env: &EnvRef,
    ) -> Result<bool, Unwind> {
        let Value::Dict(d) = subject else {
            return Ok(false);
        };
        // Evaluate the key expressions, then look each up in the dict.
        let mut matched_keys: Vec<HashKey> = Vec::with_capacity(m.keys.len());
        for (key_expr, pat) in m.keys.iter().zip(m.patterns.iter()) {
            let key_val = self.eval_expr(key_expr, env)?;
            let key = key_val.to_hash_key()?;
            let value = match d.borrow().get(&key) {
                Some(v) => v.clone(),
                None => return Ok(false),
            };
            if !self.pattern_matches(pat, &value, env)? {
                return Ok(false);
            }
            matched_keys.push(key);
        }
        if let Some(rest_name) = &m.rest {
            // Build a new dict of the keys we *didn't* consume.
            let mut rest_map: DictMap = IndexMap::new();
            for (k, v) in d.borrow().iter() {
                if !matched_keys.iter().any(|seen| seen == k) {
                    rest_map.insert(k.clone(), v.clone());
                }
            }
            env.set(
                rest_name.as_str(),
                Value::Dict(Rc::new(RefCell::new(rest_map))),
            );
        }
        Ok(true)
    }

    fn pattern_match_seq(
        &mut self,
        patterns: &[Pattern],
        items: &[Value],
        env: &EnvRef,
    ) -> Result<bool, Unwind> {
        let star_idx = patterns
            .iter()
            .position(|p| matches!(p, Pattern::MatchStar(_)));
        if let Some(s) = star_idx {
            let before = s;
            let after = patterns.len() - s - 1;
            if items.len() < before + after {
                return Ok(false);
            }
            for (p, v) in patterns[..before].iter().zip(items[..before].iter()) {
                if !self.pattern_matches(p, v, env)? {
                    return Ok(false);
                }
            }
            let mid = &items[before..items.len() - after];
            if let Pattern::MatchStar(star) = &patterns[s] {
                if let Some(name) = &star.name {
                    env.set(
                        name.as_str(),
                        Value::List(Rc::new(RefCell::new(mid.to_vec()))),
                    );
                }
            }
            for (p, v) in patterns[s + 1..]
                .iter()
                .zip(items[items.len() - after..].iter())
            {
                if !self.pattern_matches(p, v, env)? {
                    return Ok(false);
                }
            }
            Ok(true)
        } else {
            if patterns.len() != items.len() {
                return Ok(false);
            }
            for (p, v) in patterns.iter().zip(items.iter()) {
                if !self.pattern_matches(p, v, env)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
    }

    fn pattern_match_class(
        &mut self,
        c: &ast::PatternMatchClass,
        subject: &Value,
        env: &EnvRef,
    ) -> Result<bool, Unwind> {
        // Built-in type-class patterns: `case str(): ...`, `case int() as
        // n:`, etc. Match by Python type (PEP 634 §11.8.7). The single
        // positional argument is the "self" capture per PEP 634; multiple
        // positional args against a built-in type are not legal.
        if let Expr::Name(n) = c.cls.as_ref() {
            let head = n.id.as_str();
            let matched = matches!(
                (head, subject),
                ("str", Value::Str(_))
                    | ("int", Value::Int(_))
                    | ("int", Value::Bool(_))
                    | ("float", Value::Float(_))
                    | ("bool", Value::Bool(_))
                    | ("bytes", Value::Bytes(_))
                    | ("list", Value::List(_))
                    | ("tuple", Value::Tuple(_))
                    | ("dict", Value::Dict(_))
                    | ("set", Value::Set(_))
                    | ("frozenset", Value::Set(_))
            );
            if matched {
                // A single positional pattern means "bind whole subject" for
                // built-in types (PEP 634). Reject multi-positional which is
                // a class-arg-count mismatch.
                if c.arguments.patterns.len() > 1 {
                    return Ok(false);
                }
                if let Some(p) = c.arguments.patterns.first() {
                    if !self.pattern_matches(p, subject, env)? {
                        return Ok(false);
                    }
                }
                // No keyword patterns on built-ins.
                return Ok(c.arguments.keywords.is_empty());
            }
            // If we recognise the head name but didn't match, fall through —
            // the subject isn't of that built-in type.
            if matches!(
                head,
                "str"
                    | "int"
                    | "float"
                    | "bool"
                    | "bytes"
                    | "list"
                    | "tuple"
                    | "dict"
                    | "set"
                    | "frozenset"
            ) {
                return Ok(false);
            }
        }
        let cls = self.eval_expr(&c.cls, env)?;
        // Native ADTs first.
        match (&cls, subject) {
            (Value::Class(klass), Value::Instance(inst)) => {
                if !class_is_subclass(&inst.class, klass) {
                    return Ok(false);
                }
                // Positional patterns map to fields in declaration order.
                for (i, p) in c.arguments.patterns.iter().enumerate() {
                    let Some(field) = klass.fields.get(i) else {
                        return Ok(false);
                    };
                    let v = inst
                        .fields
                        .borrow()
                        .get(&field.name)
                        .cloned()
                        .unwrap_or(Value::None);
                    if !self.pattern_matches(p, &v, env)? {
                        return Ok(false);
                    }
                }
                // Keyword patterns.
                for kw in c.arguments.keywords.iter() {
                    let v = inst
                        .fields
                        .borrow()
                        .get(kw.attr.as_str())
                        .cloned()
                        .unwrap_or(Value::None);
                    if !self.pattern_matches(&kw.pattern, &v, env)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            // Match `Ok(x)` / `Err(e)` on the native ADT.
            (_, Value::ResultOk(v)) | (_, Value::ResultErr(v)) => {
                let want_ok = matches!(subject, Value::ResultOk(_));
                let name = if let Expr::Name(n) = c.cls.as_ref() {
                    Some(n.id.as_str())
                } else if let Expr::Attribute(a) = c.cls.as_ref() {
                    Some(a.attr.as_str())
                } else {
                    None
                };
                let pattern_matches =
                    matches!((name, want_ok), (Some("Ok"), true) | (Some("Err"), false));
                if !pattern_matches {
                    return Ok(false);
                }
                if let Some(p) = c.arguments.patterns.first() {
                    if !self.pattern_matches(p, v, env)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn number_to_value(n: &Number) -> Value {
    match n {
        // Ruff exposes the source-spelled int via `as_str`, which is the
        // safe path for values too big for `i64` (FINDINGS #19). Fall back
        // to `as_i64` for the common small-literal case.
        Number::Int(i) => {
            if let Some(small) = i.as_i64() {
                Value::Int(BigInt::from(small))
            } else {
                let s = format!("{i}");
                Value::Int(s.parse::<BigInt>().unwrap_or_else(|_| BigInt::from(0)))
            }
        }
        Number::Float(x) => Value::Float(*x),
        Number::Complex { .. } => Value::None, // not supported
    }
}

fn decorator_simple_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Name(n) => Some(n.id.as_str().to_owned()),
        Expr::Call(c) => decorator_simple_name(&c.func),
        Expr::Attribute(a) => Some(a.attr.as_str().to_owned()),
        _ => None,
    }
}

fn overflow() -> Unwind {
    Unwind::Exception(VmException::new("OverflowError", "int overflow"))
}

pub fn normalize_index(i: i64, len: usize) -> Option<usize> {
    let len_i = len as i64;
    let idx = if i < 0 { i + len_i } else { i };
    if idx < 0 || idx >= len_i {
        None
    } else {
        Some(idx as usize)
    }
}

/// Normalise a slice's `start`, `stop`, `step` for a sequence of length `len`
/// using CPython's algorithm (see `PySlice_AdjustIndices`). Returns signed
/// indices because negative-step slices need `stop` to be able to reach `-1`
/// (one before index 0). The caller iterates `idx += step` from `start` until
/// the appropriate boundary.
fn compute_slice(
    lower: &Value,
    upper: &Value,
    step: i64,
    len: usize,
) -> Result<(i64, i64, i64), Unwind> {
    let len_i = len as i64;
    let clamp_start = |x: i64| -> i64 {
        let x = if x < 0 { x + len_i } else { x };
        if step > 0 {
            x.clamp(0, len_i)
        } else {
            x.clamp(-1, len_i - 1)
        }
    };
    let clamp_stop = |x: i64| -> i64 {
        let x = if x < 0 { x + len_i } else { x };
        if step > 0 {
            x.clamp(0, len_i)
        } else {
            x.clamp(-1, len_i - 1)
        }
    };
    let start = match lower {
        Value::None => {
            if step > 0 {
                0
            } else {
                len_i - 1
            }
        }
        v => clamp_start(v.to_int()?),
    };
    let stop = match upper {
        Value::None => {
            if step > 0 {
                len_i
            } else {
                -1
            }
        }
        v => clamp_stop(v.to_int()?),
    };
    Ok((start, stop, step))
}

fn values_identical(a: &Value, b: &Value) -> bool {
    use Value::*;
    match (a, b) {
        (None, None) => true,
        (Bool(x), Bool(y)) => x == y,
        (Int(x), Int(y)) => x == y,
        (Str(x), Str(y)) => Rc::ptr_eq(x, y) || x == y,
        (List(x), List(y)) => Rc::ptr_eq(x, y),
        (Tuple(x), Tuple(y)) => Rc::ptr_eq(x, y),
        (Dict(x), Dict(y)) => Rc::ptr_eq(x, y),
        (Set(x), Set(y)) => Rc::ptr_eq(x, y),
        (Instance(x), Instance(y)) => Rc::ptr_eq(x, y),
        (Module(x), Module(y)) => Rc::ptr_eq(x, y),
        (Class(x), Class(y)) => Rc::ptr_eq(x, y),
        _ => false,
    }
}

/// Build a bound `NativeFn` that implements one of the four Result
/// combinators (`map`, `map_err`, `and_then`, `or_else`) over a captured
/// receiver. The four follow Rust's `Result` semantics:
///
/// * `Ok.map(f)` → `Ok(f(value))`; `Err.map(_)` → identity.
/// * `Ok.map_err(_)` → identity; `Err.map_err(g)` → `Err(g(error))`.
/// * `Ok.and_then(h)` → `h(value)` (which itself must return a `Result`);
///   `Err.and_then(_)` → identity.
/// * `Ok.or_else(_)` → identity; `Err.or_else(k)` → `k(error)`.
/// The dunder method name a binary operator dispatches to on its left operand.
fn binop_dunder(op: Operator) -> Option<&'static str> {
    Some(match op {
        Operator::Add => "__add__",
        Operator::Sub => "__sub__",
        Operator::Mult => "__mul__",
        Operator::MatMult => "__matmul__",
        Operator::Div => "__truediv__",
        Operator::FloorDiv => "__floordiv__",
        Operator::Mod => "__mod__",
        Operator::Pow => "__pow__",
        Operator::LShift => "__lshift__",
        Operator::RShift => "__rshift__",
        Operator::BitOr => "__or__",
        Operator::BitXor => "__xor__",
        Operator::BitAnd => "__and__",
    })
}

/// The reflected dunder dispatched to on the right operand when the left has none.
fn binop_reflected_dunder(op: Operator) -> Option<&'static str> {
    Some(match op {
        Operator::Add => "__radd__",
        Operator::Sub => "__rsub__",
        Operator::Mult => "__rmul__",
        Operator::MatMult => "__rmatmul__",
        Operator::Div => "__rtruediv__",
        Operator::FloorDiv => "__rfloordiv__",
        Operator::Mod => "__rmod__",
        Operator::Pow => "__rpow__",
        Operator::LShift => "__rlshift__",
        Operator::RShift => "__rrshift__",
        Operator::BitOr => "__ror__",
        Operator::BitXor => "__rxor__",
        Operator::BitAnd => "__rand__",
    })
}

fn bind_result_combinator(receiver: Value, attr: &str) -> Value {
    let combinator = match attr {
        "map" => "map",
        "map_err" => "map_err",
        "and_then" => "and_then",
        "or_else" => "or_else",
        _ => unreachable!(),
    };
    let name: &'static str = match combinator {
        "map" => "Result.map",
        "map_err" => "Result.map_err",
        "and_then" => "Result.and_then",
        "or_else" => "Result.or_else",
        _ => unreachable!(),
    };
    let nf = NativeFn::new(name, move |interp, args| {
        if args.len() != 1 {
            return Err(type_error(format!(
                "{}() takes exactly 1 argument ({} given)",
                name,
                args.len()
            )));
        }
        let f = args.into_iter().next().unwrap();
        match (&receiver, combinator) {
            (Value::ResultOk(v), "map") => {
                let mapped = interp.call_value(f, vec![(**v).clone()], &[])?;
                Ok(Value::ResultOk(Box::new(mapped)))
            }
            (Value::ResultOk(_), "map_err") => Ok(receiver.clone()),
            (Value::ResultOk(v), "and_then") => {
                let next = interp.call_value(f, vec![(**v).clone()], &[])?;
                match next {
                    Value::ResultOk(_) | Value::ResultErr(_) => Ok(next),
                    other => Err(type_error(format!(
                        "and_then closure must return Ok(...) or Err(...); got {}",
                        other.type_name()
                    ))),
                }
            }
            (Value::ResultOk(_), "or_else") => Ok(receiver.clone()),
            (Value::ResultErr(_), "map") => Ok(receiver.clone()),
            (Value::ResultErr(e), "map_err") => {
                let mapped = interp.call_value(f, vec![(**e).clone()], &[])?;
                Ok(Value::ResultErr(Box::new(mapped)))
            }
            (Value::ResultErr(_), "and_then") => Ok(receiver.clone()),
            (Value::ResultErr(e), "or_else") => {
                let next = interp.call_value(f, vec![(**e).clone()], &[])?;
                match next {
                    Value::ResultOk(_) | Value::ResultErr(_) => Ok(next),
                    other => Err(type_error(format!(
                        "or_else closure must return Ok(...) or Err(...); got {}",
                        other.type_name()
                    ))),
                }
            }
            _ => unreachable!(),
        }
    });
    Value::Native(Rc::new(nf))
}

fn class_is_subclass(c: &Rc<Class>, target: &Rc<Class>) -> bool {
    if Rc::ptr_eq(c, target) {
        return true;
    }
    c.bases.iter().any(|b| class_is_subclass(b, target))
}

/// Public wrapper so the `format()` builtin can reuse the f-string formatter.
pub fn format_with_spec_pub(value: &Value, default: &str, spec: &str) -> Result<String, Unwind> {
    format_with_spec(value, default, spec)
}

fn format_with_spec(value: &Value, default: &str, spec: &str) -> Result<String, Unwind> {
    if spec.is_empty() {
        return Ok(default.to_owned());
    }
    // Implements a useful subset of Python's PEP 3101 format mini-language:
    //   [[fill]align][sign][#][0][width][,][.precision][type]
    // Notable for FINDINGS #20: zero-pad (`0`) and alternate-form (`#`,
    // adds `0x`/`0o`/`0b` for hex/oct/bin) are now handled. Anything not
    // recognised falls through to the default string repr.
    let raw: Vec<char> = spec.chars().collect();
    let mut i = 0usize;
    let mut fill: char = ' ';
    let mut align: Option<char> = None;
    let mut sign: Option<char> = None;
    let mut alternate = false;
    let mut zero_pad = false;
    let mut width: Option<usize> = None;
    let mut precision: Option<usize> = None;
    let mut typ: Option<char> = None;
    let mut comma = false;
    let mut underscore = false;

    // Optional `[fill]align` — fill is any char *immediately followed by*
    // an alignment specifier; otherwise the first char is treated as the
    // alignment itself.
    if raw.len() >= 2 && matches!(raw[1], '<' | '>' | '^' | '=') {
        fill = raw[0];
        align = Some(raw[1]);
        i = 2;
    } else if let Some(&c) = raw.first() {
        if matches!(c, '<' | '>' | '^' | '=') {
            align = Some(c);
            i = 1;
        }
    }

    while i < raw.len() {
        let c = raw[i];
        match c {
            '+' | '-' | ' ' if sign.is_none() => {
                sign = Some(c);
                i += 1;
            }
            '#' => {
                alternate = true;
                i += 1;
            }
            '0' if width.is_none() => {
                zero_pad = true;
                i += 1;
            }
            d if d.is_ascii_digit() => {
                let mut n = 0usize;
                while i < raw.len() {
                    if let Some(k) = raw[i].to_digit(10) {
                        n = n * 10 + k as usize;
                        i += 1;
                    } else {
                        break;
                    }
                }
                width = Some(n);
            }
            ',' => {
                comma = true;
                i += 1;
            }
            '_' => {
                underscore = true;
                i += 1;
            }
            '.' => {
                i += 1;
                let mut n = 0usize;
                while i < raw.len() {
                    if let Some(k) = raw[i].to_digit(10) {
                        n = n * 10 + k as usize;
                        i += 1;
                    } else {
                        break;
                    }
                }
                precision = Some(n);
            }
            _ => {
                typ = Some(c);
                i += 1;
            }
        }
    }

    // The conversion type implies a *numeric* default alignment (right);
    // zero-pad implies fill='0' and align='=' (sign before pad). Strings
    // default to left-aligned. We approximate by tracking `is_numeric`.
    let is_numeric = matches!(value, Value::Int(_) | Value::Float(_) | Value::Bool(_));
    if zero_pad && align.is_none() {
        align = Some('=');
        fill = '0';
    }

    let buf: String;
    let mut prefix = String::new();
    let mut explicit_sign = String::new();
    match value {
        Value::Float(x) => {
            let p = precision.unwrap_or(6);
            let (abs, neg) = (x.abs(), *x < 0.0 || x.is_sign_negative());
            let raw: String = match typ {
                Some('e') => format!("{:.*e}", p, abs),
                Some('E') => format!("{:.*E}", p, abs),
                Some('g') | Some('G') => {
                    // Python's `g` uses precision as significant digits.
                    let sig = if p == 0 { 1 } else { p };
                    format!("{:.*e}", sig.saturating_sub(1), abs)
                }
                _ => format!("{:.*}", p, abs),
            };
            buf = if comma || underscore {
                let sep = if comma { ',' } else { '_' };
                insert_float_thousands(&raw, sep)
            } else {
                raw
            };
            if neg {
                explicit_sign.push('-');
            } else if let Some('+') = sign {
                explicit_sign.push('+');
            } else if let Some(' ') = sign {
                explicit_sign.push(' ');
            }
        }
        Value::Int(i_val) => {
            let abs = i_val.abs();
            let neg = i_val.is_negative();
            match typ {
                Some('x') => {
                    buf = abs.to_str_radix(16);
                    if alternate {
                        prefix.push_str("0x");
                    }
                }
                Some('X') => {
                    buf = abs.to_str_radix(16).to_uppercase();
                    if alternate {
                        prefix.push_str("0X");
                    }
                }
                Some('b') => {
                    buf = abs.to_str_radix(2);
                    if alternate {
                        prefix.push_str("0b");
                    }
                }
                Some('o') => {
                    buf = abs.to_str_radix(8);
                    if alternate {
                        prefix.push_str("0o");
                    }
                }
                _ => {
                    buf = if comma {
                        format_bigint_with_separator(&abs, ',')
                    } else if underscore {
                        format_bigint_with_separator(&abs, '_')
                    } else {
                        abs.to_str_radix(10)
                    };
                }
            }
            if neg {
                explicit_sign.push('-');
            } else if let Some('+') = sign {
                explicit_sign.push('+');
            } else if let Some(' ') = sign {
                explicit_sign.push(' ');
            }
        }
        Value::Bool(b) => {
            buf = (*b as i64).to_string();
        }
        _ => buf = default.to_owned(),
    }

    // Apply width: combine sign + prefix + body, then pad.
    if let Some(w) = width {
        let total_len =
            explicit_sign.chars().count() + prefix.chars().count() + buf.chars().count();
        if total_len < w {
            let pad = w - total_len;
            // Default alignment: numeric → right, string → left.
            let eff_align = align.unwrap_or(if is_numeric { '>' } else { '<' });
            match eff_align {
                '<' => {
                    let core = format!("{explicit_sign}{prefix}{buf}");
                    let p: String = std::iter::repeat_n(fill, pad).collect();
                    return Ok(format!("{core}{p}"));
                }
                '^' => {
                    let lo = pad / 2;
                    let hi = pad - lo;
                    let lo_s: String = std::iter::repeat_n(fill, lo).collect();
                    let hi_s: String = std::iter::repeat_n(fill, hi).collect();
                    return Ok(format!("{lo_s}{explicit_sign}{prefix}{buf}{hi_s}"));
                }
                '=' => {
                    // Pad between sign/prefix and the digits — used by
                    // zero-pad on numbers.
                    let p: String = std::iter::repeat_n(fill, pad).collect();
                    return Ok(format!("{explicit_sign}{prefix}{p}{buf}"));
                }
                _ => {
                    // '>' (default for numbers)
                    let p: String = std::iter::repeat_n(fill, pad).collect();
                    return Ok(format!("{p}{explicit_sign}{prefix}{buf}"));
                }
            }
        }
    }
    Ok(format!("{explicit_sign}{prefix}{buf}"))
}

/// Separator-group every third digit of a non-negative `BigInt`. Caller
/// is responsible for prepending the sign — the body is sign-free.
fn format_bigint_with_separator(i: &BigInt, sep: char) -> String {
    let s = i.to_str_radix(10);
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (k, c) in s.chars().rev().enumerate() {
        if k > 0 && k % 3 == 0 {
            out.push(sep);
        }
        out.push(c);
    }
    out.chars().rev().collect()
}

/// Insert `sep` between every three integer-part digits of a formatted
/// float like `"3141.59"` → `"3_141.59"` (when sep='_') or `"3,141.59"`.
/// The fractional part and any exponent are passed through verbatim.
fn insert_float_thousands(raw: &str, sep: char) -> String {
    let (int_part, rest) = match raw.find(['.', 'e', 'E']) {
        Some(idx) => (&raw[..idx], &raw[idx..]),
        None => (raw, ""),
    };
    // Honour every Python format-spec sign prefix (`-`, `+`, and
    // space). Without this, an int formatted as `f"{42:+_d}"` would
    // group as `+_42` (separator landing between sign and first
    // digit) for any total digit count that's a multiple of three.
    // Review thread gemini-code-assist on PR #147.
    let (sign, body) = if let Some(stripped) = int_part.strip_prefix('-') {
        ("-", stripped)
    } else if let Some(stripped) = int_part.strip_prefix('+') {
        ("+", stripped)
    } else if let Some(stripped) = int_part.strip_prefix(' ') {
        (" ", stripped)
    } else {
        ("", int_part)
    };
    let mut grouped = String::with_capacity(body.len() + body.len() / 3);
    for (k, c) in body.chars().rev().enumerate() {
        if k > 0 && k % 3 == 0 {
            grouped.push(sep);
        }
        grouped.push(c);
    }
    let grouped: String = grouped.chars().rev().collect();
    format!("{sign}{grouped}{rest}")
}

#[cfg(test)]
mod vm_tests {
    use super::*;
    use ruff_python_ast::ModModule;
    use tyc_syntax::parse_module;

    fn parse_and_run(src: &str) -> (Interpreter, Result<(), Unwind>) {
        let parsed = parse_module(src).expect("parse ok");
        let module: ModModule = parsed.into_syntax();
        let mut interp = Interpreter::new();
        let res = interp.run_module(&module);
        (interp, res)
    }

    /// FINDINGS #19 — `2 ** 100` and other big-integer arithmetic must
    /// not overflow now that ints are `BigInt`-backed.
    #[test]
    fn bigint_pow_no_overflow() {
        let src = r#"
result = 2 ** 100
"#;
        let (interp, res) = parse_and_run(src);
        res.unwrap();
        let v = interp.root.get("result").expect("defined");
        assert_eq!(v.py_str(), "1267650600228229401496703205376");
    }

    #[test]
    fn bigint_fib_99_no_overflow() {
        let src = r#"
def fib(n):
    if n < 2:
        return n
    a = 0
    b = 1
    for _ in range(n - 1):
        a, b = b, a + b
    return b

result = fib(99)
"#;
        let (interp, res) = parse_and_run(src);
        res.unwrap();
        let v = interp.root.get("result").expect("defined");
        // fib(99) = 218922995834555169026
        assert_eq!(v.py_str(), "218922995834555169026");
    }

    /// FINDINGS #20 — zero-pad, alternate-form, and width+precision for
    /// f-string format specs must match Python.
    #[test]
    fn fstring_zero_pad_and_alternate_form() {
        let v = Value::Int(BigInt::from(42));
        assert_eq!(format_with_spec(&v, "42", "04d").unwrap(), "0042");
        assert_eq!(format_with_spec(&v, "42", "06").unwrap(), "000042");
        assert_eq!(format_with_spec(&v, "42", "#x").unwrap(), "0x2a");
        assert_eq!(format_with_spec(&v, "42", "#o").unwrap(), "0o52");
        assert_eq!(format_with_spec(&v, "42", "#b").unwrap(), "0b101010");
        // The findings note used `0003.142`, but CPython actually rounds
        // `3.14` at precision 3 to `3.140`; matching Python's true output.
        // Use literals that aren't approximations of well-known math
        // constants — clippy::approx_constant fires on `3.14` / `2.71828`
        // even in test code. The format-spec parser doesn't care about
        // the value's identity, only its digits.
        let approx_pi = Value::Float(3.140_001);
        assert_eq!(
            format_with_spec(&approx_pi, "3.14", "08.3f").unwrap(),
            "0003.140"
        );
        // Test a value where the third decimal is meaningful.
        let approx_e = Value::Float(2.718_25);
        assert_eq!(
            format_with_spec(&approx_e, "2.71828", "08.3f").unwrap(),
            "0002.718"
        );
        // Combined: alternate-form hex with zero-pad and width.
        assert_eq!(format_with_spec(&v, "42", "#06x").unwrap(), "0x002a");
        // Negative numbers respect sign position with zero-pad.
        let n = Value::Int(BigInt::from(-7));
        assert_eq!(format_with_spec(&n, "-7", "04d").unwrap(), "-007");
        // Default formatting still works.
        assert_eq!(format_with_spec(&v, "42", "5").unwrap(), "   42");
        assert_eq!(format_with_spec(&v, "42", "<5").unwrap(), "42   ");
    }

    /// FINDINGS #30 — mapping patterns `case {"k": v}` must match
    /// against dicts and bind nested patterns.
    #[test]
    fn match_mapping_pattern_binds_value() {
        let src = r#"
shape = {"type": "circle", "radius": 7}
def kind(s):
    match s:
        case {"type": "circle", "radius": r}:
            return ("circle", r)
        case {"type": t}:
            return ("other", t)
        case _:
            return ("unknown", 0)
result = kind(shape)
"#;
        let (interp, res) = parse_and_run(src);
        res.unwrap();
        let v = interp.root.get("result").expect("defined");
        assert_eq!(v.py_str(), "('circle', 7)");
    }

    #[test]
    fn match_mapping_pattern_with_rest() {
        let src = r#"
d = {"a": 1, "b": 2, "c": 3}
def f(x):
    match x:
        case {"a": a, **rest}:
            return (a, rest)
        case _:
            return (0, {})
result = f(d)
"#;
        let (interp, res) = parse_and_run(src);
        res.unwrap();
        let v = interp.root.get("result").expect("defined");
        // The remaining dict should include "b" and "c" in insertion order.
        assert_eq!(v.py_str(), "(1, {'b': 2, 'c': 3})");
    }

    /// FINDINGS #30 — sequence pattern with star: `case [x, *rest, y]`.
    /// Already worked in v1; this is a regression-guard test.
    #[test]
    fn match_sequence_pattern_with_star() {
        let src = r#"
xs = [1, 2, 3, 4, 5]
def f(s):
    match s:
        case [first, *middle, last]:
            return (first, middle, last)
        case _:
            return (0, [], 0)
result = f(xs)
"#;
        let (interp, res) = parse_and_run(src);
        res.unwrap();
        let v = interp.root.get("result").expect("defined");
        assert_eq!(v.py_str(), "(1, [2, 3, 4], 5)");
    }

    /// FINDINGS #31 — recursion limit raised to match Python's default
    /// of 1000.
    #[test]
    fn recursion_limit_matches_python_default() {
        let interp = Interpreter::new();
        assert!(
            interp.max_stack_depth >= 1000,
            "max_stack_depth should be >= 1000 (Python default), got {}",
            interp.max_stack_depth
        );
    }

    /// FINDINGS #28 / #29 — VM doesn't support generators or async, but
    /// the error message must point users at the `tyc build && python`
    /// workaround.
    #[test]
    fn yield_error_mentions_tyc_build_fallback() {
        let src = r#"
def gen():
    yield 1
gen()
"#;
        let (_interp, res) = parse_and_run(src);
        let err = res.expect_err("yield should error");
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("tyc build") && msg.contains("python"),
            "yield error should mention `tyc build` + `python` fallback, got: {msg}"
        );
    }

    #[test]
    fn async_def_error_mentions_tyc_build_fallback() {
        let src = r#"
async def fetch():
    return 1
fetch()
"#;
        let (_interp, res) = parse_and_run(src);
        let err = res.expect_err("async should error");
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("tyc build") && msg.contains("python"),
            "async error should mention `tyc build` + `python` fallback, got: {msg}"
        );
    }

    /// FINDINGS #18 — `IndexMap` preserves insertion order so `tyc run`
    /// and `tyc build && python` produce identical output for any
    /// dict-printing program.
    #[test]
    fn dict_preserves_insertion_order() {
        let src = r#"
d = {"a": 1, "b": 2, "c": 3, "d": 4, "e": 5}
"#;
        let (interp, res) = parse_and_run(src);
        res.unwrap();
        let v = interp.root.get("d").expect("d defined");
        let Value::Dict(d) = v else {
            panic!("expected dict, got {:?}", v);
        };
        let keys: Vec<String> = d
            .borrow()
            .keys()
            .map(|k| k.clone().into_value().py_str())
            .collect();
        assert_eq!(keys, vec!["a", "b", "c", "d", "e"]);
        // py_str on the dict must match Python's output.
        assert_eq!(
            Value::Dict(d).py_str(),
            "{'a': 1, 'b': 2, 'c': 3, 'd': 4, 'e': 5}"
        );
    }
}
