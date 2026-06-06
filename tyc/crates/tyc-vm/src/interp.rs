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
    /// Stack of value buffers for generator functions. The VM has no way to
    /// suspend a tree-walk mid-frame, so a `yield`-bearing function is run
    /// eagerly to completion with each yielded value pushed onto the top
    /// buffer; the call then returns an iterator over the collected values.
    /// A stack supports nested/recursive generators. See `GENERATOR_CAP`.
    pub gen_buffers: Vec<Vec<Value>>,
    /// Names of modules currently being loaded via `try_load_typhon_module`.
    /// Guards against a module that imports itself (directly or through a
    /// cycle) re-entering the loader and overflowing the host stack — e.g.
    /// a file named `enum.ty` that does `from enum import Enum` would
    /// otherwise try to load itself forever.
    pub loading_modules: std::collections::HashSet<String>,
    /// Stack of `(defining_class, self_value)` frames for in-flight method
    /// calls. Lets zero-arg `super()` resolve the next method up the MRO
    /// and bind `self`. Pushed when a bound method / `__call__` /
    /// `__post_init__` is invoked, popped on the way out.
    pub method_stack: Vec<(Rc<Class>, Value)>,
    /// Stack of exceptions currently being handled (one frame per active
    /// `except` body). A bare `raise` re-raises the top of this stack.
    pub active_exceptions: Vec<VmException>,
}

/// Upper bound on values an eagerly-evaluated generator may yield before the
/// VM gives up. Bounds the worst case (an unbounded `while True: yield`) to a
/// clear error instead of an unbounded hang; `tyc build` runs such generators
/// lazily on CPython.
const GENERATOR_CAP: usize = 1_000_000;

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
            gen_buffers: Vec::new(),
            loading_modules: std::collections::HashSet::new(),
            method_stack: Vec::new(),
            active_exceptions: Vec::new(),
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
                if self.is_truthy(&cond)? {
                    self.exec_block(&s.body, env)?;
                } else {
                    for clause in &s.elif_else_clauses {
                        match &clause.test {
                            Some(test) => {
                                let c = self.eval_expr(test, env)?;
                                if self.is_truthy(&c)? {
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
                    if !self.is_truthy(&cond)? {
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
                    Some(e) => {
                        let v = self.eval_expr(e, env)?;
                        // `raise SomeError` (a bare class / builtin exception
                        // constructor) instantiates it, matching Python — so
                        // `raise StopIteration` yields a StopIteration value,
                        // not the constructor function.
                        match v {
                            Value::Native(_) => self.call_value(v, vec![], &[])?,
                            Value::Class(ref c) => self.instantiate(c, vec![], &[])?,
                            other => other,
                        }
                    }
                    None => {
                        // Bare `raise` re-raises the exception currently being
                        // handled by the innermost enclosing `except` block.
                        return match self.active_exceptions.last() {
                            Some(active) => Err(Unwind::Exception(active.clone())),
                            None => Err(Unwind::Exception(VmException::new(
                                "RuntimeError",
                                "No active exception to re-raise",
                            ))),
                        };
                    }
                };
                Err(self.value_to_exception(exc))
            }
            Stmt::Try(t) => self.exec_try(t, env),
            Stmt::Match(m) => self.exec_match(m, env),
            Stmt::Assert(a) => {
                let cond = self.eval_expr(&a.test, env)?;
                if !self.is_truthy(&cond)? {
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
                        // `del obj.attr` removes an instance attribute.
                        Expr::Attribute(a) => {
                            let recv = self.eval_expr(&a.value, env)?;
                            match recv {
                                Value::Instance(inst) => {
                                    if inst.fields.borrow_mut().remove(a.attr.as_str()).is_none() {
                                        return Err(attribute_error(format!(
                                            "{}",
                                            a.attr.as_str()
                                        )));
                                    }
                                }
                                _ => {
                                    return Err(type_error(
                                        "cannot delete attribute on this object",
                                    ))
                                }
                            }
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

        // A class is dataclass-shaped (annotated assigns are instance fields)
        // when it carries a `@dataclass` decorator. A `plain class` emits a
        // bare class with no decorator, so its annotated assigns with a
        // default are class attributes, not slots. `ClassVar[...]` is always
        // a class attribute regardless of class kind.
        let rightmost_name = |e: &Expr| -> Option<String> {
            match e {
                Expr::Name(n) => Some(n.id.as_str().to_owned()),
                Expr::Attribute(a) => Some(a.attr.as_str().to_owned()),
                Expr::Call(call) => match call.func.as_ref() {
                    Expr::Name(n) => Some(n.id.as_str().to_owned()),
                    Expr::Attribute(a) => Some(a.attr.as_str().to_owned()),
                    _ => None,
                },
                _ => None,
            }
        };
        let is_dataclass = c
            .decorator_list
            .iter()
            .any(|d| rightmost_name(&d.expression).as_deref() == Some("dataclass"));
        let is_classvar = |ann: &Expr| -> bool {
            let head = match ann {
                Expr::Subscript(s) => s.value.as_ref(),
                other => other,
            };
            matches!(head, Expr::Name(n) if n.id.as_str() == "ClassVar")
                || matches!(head, Expr::Attribute(a) if a.attr.as_str() == "ClassVar")
        };

        let mut fields = Vec::new();
        let mut methods: HashMap<String, Rc<Function>> = HashMap::new();
        let mut class_attrs: HashMap<String, Value> = HashMap::new();
        let mut properties: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut classmethods: std::collections::HashSet<String> = std::collections::HashSet::new();

        for stmt in &c.body {
            match stmt {
                Stmt::FunctionDef(f) => {
                    let func = self.build_function(f, &body_env)?;
                    // `@prop.setter` / `@prop.deleter` register the decorated
                    // function as the property's setter/deleter (under a
                    // sentinel class-attr key) rather than overriding the
                    // getter method. Evaluating `prop.setter` directly would
                    // fail (`prop` isn't bound during class-body execution).
                    let mut setter_target: Option<String> = None;
                    for deco in f.decorator_list.iter() {
                        if let Expr::Attribute(a) = &deco.expression {
                            if a.attr.as_str() == "setter" {
                                if let Expr::Name(prop) = a.value.as_ref() {
                                    setter_target = Some(prop.id.as_str().to_owned());
                                }
                            }
                        }
                    }
                    if let Some(prop) = setter_target {
                        class_attrs.insert(
                            format!("__typhon_setter__{}", prop),
                            Value::Function(Rc::new(func)),
                        );
                        continue;
                    }
                    let mut v = Value::Function(Rc::new(func));
                    let has_deco = |want: &str| {
                        f.decorator_list.iter().any(
                            |d| matches!(&d.expression, Expr::Name(n) if n.id.as_str() == want),
                        )
                    };
                    // `@cached_property` behaves like a property on read (the
                    // VM invokes it lazily). True per-instance caching is not
                    // modelled, but the observable value is correct.
                    let is_property = has_deco("property") || has_deco("cached_property");
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
                        let classvar = is_classvar(&a.annotation);
                        // ClassVar is always a class attribute. In a non-
                        // dataclass (`plain class`) an annotated assign with a
                        // default is also a class attribute, not an instance
                        // slot. Everything else is a dataclass field.
                        if classvar || (!is_dataclass && default.is_some()) {
                            if let Some(v) = default {
                                class_attrs.insert(n.id.as_str().to_owned(), v);
                            }
                        } else {
                            fields.push(ClassField {
                                name: n.id.as_str().to_owned(),
                                default,
                            });
                        }
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

        // Names the subclass defines itself — an inherited descriptor marker
        // (`@property` / `@classmethod`) must NOT carry over to a name the
        // subclass has overridden with a plain method.
        let own_method_names: std::collections::HashSet<String> = methods.keys().cloned().collect();

        // Inherit methods from base classes that aren't already overridden.
        for base in &bases {
            for (name, m) in base.methods.borrow().iter() {
                methods.entry(name.clone()).or_insert_with(|| m.clone());
            }
            for (name, v) in base.class_attrs.borrow().iter() {
                // Don't inherit the internal enum sentinels — a subclass
                // should not be treated as the enum base marker, and each
                // enum owns its own member list (materialised after this
                // class is built).
                if is_enum_sentinel(name) {
                    continue;
                }
                class_attrs.entry(name.clone()).or_insert_with(|| v.clone());
            }
            for name in base.properties.borrow().iter() {
                if !own_method_names.contains(name) {
                    properties.insert(name.clone());
                }
            }
            for name in base.classmethods.borrow().iter() {
                if !own_method_names.contains(name) {
                    classmethods.insert(name.clone());
                }
            }
        }

        // Accumulate annotated fields across the FULL inheritance chain so a
        // synthesised constructor for `C(B(A))` knows about A's and B's
        // fields too — not just the single nearest hop. Base fields come
        // first (matching dataclass field order); a field redeclared on a
        // subclass keeps the subclass's position/default.
        let mut inherited: Vec<ClassField> = Vec::new();
        let mut seen: std::collections::HashSet<String> =
            fields.iter().map(|f| f.name.clone()).collect();
        for base in &bases {
            for bf in Self::collect_mro_fields(base) {
                if seen.insert(bf.name.clone()) {
                    inherited.push(bf);
                }
            }
        }
        // Prepend inherited fields (parents before children).
        let mut all_fields = inherited;
        all_fields.extend(fields);
        let fields = all_fields;

        let class = Rc::new(Class {
            name: c.name.as_str().to_owned(),
            methods: RefCell::new(methods),
            fields,
            class_attrs: RefCell::new(class_attrs),
            bases,
            properties: RefCell::new(properties),
            classmethods: RefCell::new(classmethods),
        });

        // Enum subclass: convert simple class-level assignments (`RED = 1`)
        // into singleton member instances carrying `.name` / `.value`, and
        // record their definition order so `for c in Color` iterates them.
        // Done after the `Class` exists so each member can be an `Instance`
        // of it; skipped for the bare base marker itself.
        if Self::is_enum_class(&class)
            && !class
                .class_attrs
                .borrow()
                .contains_key("__typhon_enum_base__")
        {
            self.materialise_enum_members(&class, c);
        }

        Ok(class)
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
        // `enum` is provided natively so that `from enum import Enum`
        // resolves to a real `Enum` base class with member/value/name
        // semantics — and crucially never falls through to loading a
        // sibling `enum.ty` (which, for the repro file literally named
        // `enum.ty`, would recursively import itself and overflow the
        // host stack).
        if name == "enum" {
            let m = self.make_enum_module();
            self.module_cache.insert(name.to_owned(), m.clone());
            return Ok(m);
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

        // Guard against a self-importing module (a file that imports its
        // own module name, directly or via a cycle). Without this the
        // loader recurses until the host stack overflows.
        if self.loading_modules.contains(name) {
            return Err(crate::error::Unwind::Exception(VmException::new(
                "ImportError",
                format!("cannot import '{name}': circular import"),
            )));
        }
        self.loading_modules.insert(name.to_owned());
        let result = self.try_load_typhon_module_inner(name, &path);
        self.loading_modules.remove(name);
        result
    }

    fn try_load_typhon_module_inner(
        &mut self,
        name: &str,
        path: &std::path::Path,
    ) -> Result<Option<Value>, Unwind> {
        let source = std::fs::read_to_string(path).map_err(|e| {
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

        // `pub *` in a package's `__init__.ty` aggregates every sibling
        // module's `pub` names (and direct sub-packages) into the package
        // namespace, so `from pkg import f` resolves under `tyc run` the same
        // way it does after `tyc build`.
        if path.file_name().and_then(|n| n.to_str()) == Some("__init__.ty")
            && !prep.pub_star_lines.is_empty()
        {
            if let Some(pkg_dir) = path.parent().map(|p| p.to_path_buf()) {
                let prefix = if name.is_empty() {
                    String::new()
                } else {
                    format!("{name}.")
                };
                let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(&pkg_dir)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                    .collect();
                entries.sort();
                for p in entries {
                    // Sibling module `pkg/mod.ty`, or sub-package
                    // `pkg/sub/__init__.ty`.
                    let sub_name = if p.extension().and_then(|e| e.to_str()) == Some("ty") {
                        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                        if stem == "__init__" || stem.is_empty() {
                            continue;
                        }
                        stem.to_owned()
                    } else if p.is_dir() && p.join("__init__.ty").exists() {
                        p.file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_owned()
                    } else {
                        continue;
                    };
                    if sub_name.is_empty() {
                        continue;
                    }
                    // The sibling's public surface: its `pub` names (for a
                    // sub-package, whatever its own `__init__` exposes).
                    let pub_names = read_pub_names(&p);
                    let submod = self.import_module(&format!("{prefix}{sub_name}"))?;
                    if pub_names.is_empty() {
                        // A sub-package with `pub *` exposes everything it
                        // aggregated — re-export all its members.
                        if p.is_dir() {
                            if let Value::Module(m) = &submod {
                                for (k, v) in m.members.borrow().iter() {
                                    members.entry(k.clone()).or_insert_with(|| v.clone());
                                }
                            }
                        }
                        continue;
                    }
                    for pn in pub_names {
                        if let Ok(v) = self.get_attr(&submod, &pn) {
                            members.entry(pn).or_insert(v);
                        }
                    }
                }
            }
        }

        Ok(Some(Value::Module(Rc::new(Module {
            name: name.to_owned(),
            members: RefCell::new(members),
        }))))
    }

    /// Build a native `enum` module exposing `Enum` (and the common
    /// variants) as base classes. Each carries a sentinel class attr
    /// (`__typhon_enum_base__`) so `build_class` can recognise a subclass
    /// and convert its simple class-level assignments (`RED = 1`) into
    /// singleton member instances with `.name` / `.value`.
    fn make_enum_module(&mut self) -> Value {
        use crate::value::Module;
        let mut members: HashMap<String, Value> = HashMap::new();
        for base_name in ["Enum", "IntEnum", "StrEnum", "Flag", "IntFlag"] {
            let mut class_attrs: HashMap<String, Value> = HashMap::new();
            class_attrs.insert("__typhon_enum_base__".to_owned(), Value::Bool(true));
            let cls = Rc::new(Class {
                name: base_name.to_owned(),
                methods: RefCell::new(HashMap::new()),
                fields: vec![],
                class_attrs: RefCell::new(class_attrs),
                bases: vec![],
                properties: RefCell::new(std::collections::HashSet::new()),
                classmethods: RefCell::new(std::collections::HashSet::new()),
            });
            members.insert(base_name.to_owned(), Value::Class(cls));
        }
        // `enum.auto()` — returns a sentinel that `materialise_enum_members`
        // replaces with a sequential integer (1, 2, 3, …) in member order,
        // matching CPython's `auto()`.
        let auto = NativeFn::new("enum.auto", |_i, _args| {
            Ok(Value::Tuple(Rc::new(vec![Value::Str(Rc::new(
                "__typhon_enum_auto__".to_owned(),
            ))])))
        });
        members.insert("auto".to_owned(), Value::Native(Rc::new(auto)));
        Value::Module(Rc::new(Module {
            name: "enum".to_owned(),
            members: RefCell::new(members),
        }))
    }

    /// True when `v` is the `enum.auto()` sentinel.
    fn is_enum_auto(v: &Value) -> bool {
        matches!(v, Value::Tuple(t) if t.len() == 1
            && matches!(&t[0], Value::Str(s) if s.as_str() == "__typhon_enum_auto__"))
    }

    /// True when `class` (or any base) is the native `enum.Enum` marker
    /// base class.
    fn is_enum_class(class: &Rc<Class>) -> bool {
        if class
            .class_attrs
            .borrow()
            .contains_key("__typhon_enum_base__")
        {
            return true;
        }
        class.bases.iter().any(Self::is_enum_class)
    }

    /// True when `value` is an enum *member* instance (an `Instance` of an
    /// enum class carrying the synthesised `_name_` field).
    /// The materialised member list for an enum class, if any.
    fn enum_members(class: &Rc<Class>) -> Option<Vec<Value>> {
        match class.class_attrs.borrow().get("__typhon_enum_members__") {
            Some(Value::List(l)) => Some(l.borrow().clone()),
            _ => None,
        }
    }

    /// `Color(value)` — look an enum member up by its value (CPython
    /// semantics: returns the existing singleton, or raises `ValueError`).
    /// Passing an existing member of the same enum returns it unchanged.
    fn enum_lookup_by_value(
        &mut self,
        class: &Rc<Class>,
        args: Vec<Value>,
        kwargs: &[(String, Value)],
    ) -> Result<Value, Unwind> {
        if !kwargs.is_empty() || args.len() != 1 {
            return Err(type_error(format!(
                "{}() takes exactly 1 argument ({} given)",
                class.name,
                args.len()
            )));
        }
        let needle = &args[0];
        // An existing member of this enum is returned as-is.
        if let Value::Instance(i) = needle {
            if Rc::ptr_eq(&i.class, class) {
                return Ok(needle.clone());
            }
        }
        if let Some(members) = Self::enum_members(class) {
            for m in &members {
                if let Value::Instance(i) = m {
                    if let Some(v) = i.fields.borrow().get("_value_") {
                        if v.py_eq(needle) {
                            return Ok(m.clone());
                        }
                    }
                }
            }
        }
        Err(value_error(format!(
            "{} is not a valid {}",
            needle.py_repr(),
            class.name
        )))
    }

    /// `Color["RED"]` — look an enum member up by name (raises `KeyError`).
    fn enum_lookup_by_name(&self, class: &Rc<Class>, name: &str) -> Result<Value, Unwind> {
        if let Some(members) = Self::enum_members(class) {
            for m in &members {
                if let Value::Instance(i) = m {
                    if let Some(Value::Str(n)) = i.fields.borrow().get("_name_") {
                        if n.as_str() == name {
                            return Ok(m.clone());
                        }
                    }
                }
            }
        }
        Err(key_error(format!("'{}'", name)))
    }

    fn is_enum_member(value: &Value) -> bool {
        if let Value::Instance(i) = value {
            return Self::is_enum_class(&i.class) && i.fields.borrow().contains_key("_name_");
        }
        false
    }

    /// Replace an enum subclass's plain value attributes with member
    /// instances. The source order is taken from the class body so
    /// iteration matches CPython.
    fn materialise_enum_members(&mut self, class: &Rc<Class>, c: &ast::StmtClassDef) {
        // Collect member names in source order (each `NAME = value`
        // assignment in the class body, excluding dunders).
        let mut order: Vec<String> = Vec::new();
        for stmt in &c.body {
            if let Stmt::Assign(a) = stmt {
                for t in &a.targets {
                    if let Expr::Name(n) = t {
                        let name = n.id.as_str();
                        if !name.starts_with("__") {
                            order.push(name.to_owned());
                        }
                    }
                }
            }
        }
        let mut member_list: Vec<Value> = Vec::with_capacity(order.len());
        {
            let mut attrs = class.class_attrs.borrow_mut();
            // Tracks the last assigned integer value so a bare `auto()`
            // continues from it (CPython: `A = 10; B = auto()` ⇒ `B == 11`).
            // Starts at 0 so a leading `auto()` yields 1.
            let mut last_value: i64 = 0;
            for name in &order {
                let Some(raw) = attrs.get(name).cloned() else {
                    continue;
                };
                // Already a member (e.g. inherited) — skip.
                if matches!(&raw, Value::Instance(i) if Rc::ptr_eq(&i.class, class)) {
                    continue;
                }
                // `enum.auto()` → previous value + 1; an explicit integer
                // value advances the counter so following `auto()`s continue
                // from it.
                let raw = if Self::is_enum_auto(&raw) {
                    last_value += 1;
                    Value::Int(BigInt::from(last_value))
                } else {
                    if let Value::Int(i) = &raw {
                        if let Some(v) = i.to_i64() {
                            last_value = v;
                        }
                    }
                    raw
                };
                let mut fields: HashMap<String, Value> = HashMap::new();
                fields.insert("name".to_owned(), Value::Str(Rc::new(name.clone())));
                fields.insert("_name_".to_owned(), Value::Str(Rc::new(name.clone())));
                fields.insert("value".to_owned(), raw.clone());
                fields.insert("_value_".to_owned(), raw.clone());
                let member = Value::Instance(Rc::new(Instance {
                    class: class.clone(),
                    fields: RefCell::new(fields),
                }));
                attrs.insert(name.clone(), member.clone());
                member_list.push(member);
            }
            attrs.insert(
                "__typhon_enum_members__".to_owned(),
                Value::List(Rc::new(RefCell::new(member_list))),
            );
        }
    }

    /// Fields of `class` including everything it inherited. `build_class`
    /// already accumulates ancestor fields onto each class, so this is
    /// normally the class's own (merged) field list — but we recurse
    /// defensively for any base built without merging.
    fn collect_mro_fields(class: &Rc<Class>) -> Vec<ClassField> {
        if !class.fields.is_empty() {
            return class.fields.clone();
        }
        let mut out = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for base in &class.bases {
            for f in Self::collect_mro_fields(base) {
                if seen.insert(f.name.clone()) {
                    out.push(f);
                }
            }
        }
        out
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
                            if !self.is_truthy(&val)? {
                                return Ok(val);
                            }
                            last = val;
                        }
                        BoolOp::Or => {
                            if self.is_truthy(&val)? {
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
                    // `{*xs, 9}` — splat an iterable's elements into the set.
                    if let Expr::Starred(st) = e {
                        let it = self.eval_expr(&st.value, env)?;
                        let it = self.make_iter(it)?;
                        while let Some(v) = self.iter_next(&it)? {
                            set.insert(v.to_hash_key()?);
                        }
                    } else {
                        let v = self.eval_expr(e, env)?;
                        set.insert(v.to_hash_key()?);
                    }
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
                if self.is_truthy(&cond)? {
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
            Expr::Yield(y) => {
                let v = match &y.value {
                    Some(e) => self.eval_expr(e, env)?,
                    None => Value::None,
                };
                self.push_yield(v)?;
                // `send()` is unsupported in eager mode — a yield expression
                // evaluates to None (the common `yield x`-as-statement case).
                Ok(Value::None)
            }
            Expr::YieldFrom(y) => {
                let iterable = self.eval_expr(&y.value, env)?;
                let it = self.make_iter(iterable)?;
                while let Some(v) = self.iter_next(&it)? {
                    self.push_yield(v)?;
                }
                Ok(Value::None)
            }
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
                                // PEP 501 self-documenting `=` debug
                                // specifier: `f"{val=}"` emits the verbatim
                                // source text (the expression, `=`, and any
                                // surrounding whitespace, stored on
                                // `debug_text`) before the value. With no
                                // explicit conversion or format spec the
                                // value renders via `repr`, matching CPython.
                                let has_debug = interp.debug_text.is_some();
                                if let Some(dt) = &interp.debug_text {
                                    out.push_str(dt.as_str());
                                }
                                let s = match interp.conversion {
                                    ast::ConversionFlag::Repr => self.repr_of(&v)?,
                                    ast::ConversionFlag::Ascii => self.repr_of(&v)?,
                                    ast::ConversionFlag::Str => self.str_of(&v)?,
                                    ast::ConversionFlag::None => {
                                        if has_debug && interp.format_spec.is_none() {
                                            self.repr_of(&v)?
                                        } else {
                                            self.str_of(&v)?
                                        }
                                    }
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

    /// `a == b` honouring a user `__eq__` on instances (used by `list.index`,
    /// `list.count`, `list.remove`, and membership tests).
    pub fn values_equal(&mut self, a: &Value, b: &Value) -> Result<bool, Unwind> {
        self.cmp_op(CmpOp::Eq, a, b)
    }

    /// Total ordering honouring a user `__lt__` / `__eq__` on instances (used
    /// by `list.sort` / `sorted` so custom comparison dunders take effect).
    pub fn value_cmp(&mut self, a: &Value, b: &Value) -> Result<std::cmp::Ordering, Unwind> {
        use std::cmp::Ordering;
        if matches!(a, Value::Instance(_)) || matches!(b, Value::Instance(_)) {
            if self.cmp_op(CmpOp::Lt, a, b)? {
                return Ok(Ordering::Less);
            }
            if self.cmp_op(CmpOp::Eq, a, b)? {
                return Ok(Ordering::Equal);
            }
            return Ok(Ordering::Greater);
        }
        Ok(a.py_cmp(b).unwrap_or(Ordering::Equal))
    }

    fn cmp_op(&mut self, op: CmpOp, l: &Value, r: &Value) -> Result<bool, Unwind> {
        use std::cmp::Ordering::*;
        // User-defined rich comparisons take priority when an operand is a
        // class instance (Python's `__eq__` / `__lt__` / … protocol).
        if matches!(l, Value::Instance(_)) || matches!(r, Value::Instance(_)) {
            if let Some(b) = self.cmp_dunder(op, l, r)? {
                return Ok(b);
            }
            // Python derives `!=` from `__eq__` when `__ne__` is absent.
            if op == CmpOp::NotEq {
                if let Some(b) = self.cmp_dunder(CmpOp::Eq, l, r)? {
                    return Ok(!b);
                }
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
            Value::DictView { items, .. } => {
                let items = items.clone();
                for v in &items {
                    if self.cmp_op(CmpOp::Eq, v, item)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
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
        // `super().method(...)` / `super(Cls, self).method(...)` — resolve
        // the next method up the MRO and bind `self`. Handled here (rather
        // than via a `Value::Super`) because the VM has no super-proxy type.
        if let Expr::Attribute(a) = c.func.as_ref() {
            if let Some(sup) = Self::as_super_call(&a.value) {
                return self.eval_super_method_call(sup, a.attr.as_str(), c, env);
            }
        }
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

    /// Recognise an expression that is a call to `super(...)`. Returns that
    /// call when matched.
    fn as_super_call(expr: &Expr) -> Option<&ast::ExprCall> {
        if let Expr::Call(call) = expr {
            if let Expr::Name(n) = call.func.as_ref() {
                if n.id.as_str() == "super" {
                    return Some(call);
                }
            }
        }
        None
    }

    /// Resolve `super(...).attr(args)`. Supports zero-arg `super()` (uses the
    /// in-flight method frame) and two-arg `super(Cls, self)`.
    fn eval_super_method_call(
        &mut self,
        sup: &ast::ExprCall,
        attr: &str,
        outer: &ast::ExprCall,
        env: &EnvRef,
    ) -> Result<Value, Unwind> {
        // Determine the class to search *above* and the bound `self`.
        let (start_class, self_val) = if sup.arguments.args.is_empty() {
            // Zero-arg: pull from the current method frame.
            self.method_stack
                .last()
                .cloned()
                .ok_or_else(|| type_error("super(): no arguments and no enclosing method"))?
        } else {
            // Two-arg form `super(Cls, obj)`. Any other arity is a `TypeError`
            // in CPython (`super(A, b, c)`); reject it rather than silently
            // ignoring the extra arguments.
            if sup.arguments.args.len() != 2 {
                return Err(type_error("super() takes 0 or 2 arguments"));
            }
            let cls_v = self.eval_expr(&sup.arguments.args[0], env)?;
            let obj_v = self.eval_expr(&sup.arguments.args[1], env)?;
            match cls_v {
                Value::Class(c) => (c, obj_v),
                _ => return Err(type_error("super() argument 1 must be a class")),
            }
        };

        // Resolve the method starting from the bases of `start_class`.
        let method = start_class
            .bases
            .iter()
            .find_map(|b| self.find_method(b, attr));
        let Some(method) = method else {
            // `super().__init__()` against a base with no such method (e.g.
            // the implicit `object`) is a no-op, matching CPython for the
            // synthesised dataclass constructors.
            if attr == "__init__" {
                return Ok(Value::None);
            }
            return Err(attribute_error(format!(
                "'super' object has no attribute '{}'",
                attr
            )));
        };

        // Evaluate the call arguments.
        let mut args = Vec::with_capacity(outer.arguments.args.len());
        for arg in outer.arguments.args.iter() {
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
        let mut kwargs: Vec<(String, Value)> = Vec::with_capacity(outer.arguments.keywords.len());
        for kw in outer.arguments.keywords.iter() {
            if let Some(name) = &kw.arg {
                kwargs.push((name.as_str().to_owned(), self.eval_expr(&kw.value, env)?));
            }
        }

        // Find the class that actually owns `method` so a chained
        // `super()` inside it resolves the *next* level up, not itself.
        let owner = start_class
            .bases
            .iter()
            .find_map(|b| self.method_owner(b, &method))
            .unwrap_or_else(|| start_class.clone());
        self.call_method_with_frame(&method, owner, self_val, args, &kwargs)
    }

    /// Walk `class`'s MRO and return the class whose method table holds
    /// `func` (by pointer identity).
    fn method_owner(&self, class: &Rc<Class>, func: &Rc<Function>) -> Option<Rc<Class>> {
        if let Some(m) = class.methods.borrow().get(&func.name) {
            if Rc::ptr_eq(m, func) {
                return Some(class.clone());
            }
        }
        for base in &class.bases {
            if let Some(c) = self.method_owner(base, func) {
                return Some(c);
            }
        }
        None
    }

    /// Call `method` bound to `self_val`, pushing a `(owner, self)` frame so
    /// a zero-arg `super()` inside it can climb the MRO.
    fn call_method_with_frame(
        &mut self,
        method: &Rc<Function>,
        owner: Rc<Class>,
        self_val: Value,
        args: Vec<Value>,
        kwargs: &[(String, Value)],
    ) -> Result<Value, Unwind> {
        self.method_stack.push((owner, self_val.clone()));
        let mut full_args = Vec::with_capacity(args.len() + 1);
        full_args.push(self_val);
        full_args.extend(args);
        let result = self.call_function(method, full_args, kwargs, None);
        self.method_stack.pop();
        result
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
                    // The generic bound builtin-method dispatcher (`obj.sort`,
                    // `obj.method`) can't see kwargs through its fixed native
                    // signature, so forward them as a trailing sentinel arg the
                    // method handlers unpack. Other natives use the by-name path.
                    if n.name == "method" {
                        let mut args = args;
                        args.push(crate::builtins::make_kwargs_sentinel(kwargs));
                        return (n.func)(self, args);
                    }
                    // For v1, native fns receive positional args only.
                    // Special-case common kwarg-accepting builtins (sorted, dict.get) by name.
                    return crate::builtins::call_with_kwargs(self, &n, args, kwargs);
                }
                (n.func)(self, args)
            }
            Value::Function(f) => self.call_function(&f, args, kwargs, None),
            Value::BoundMethod { receiver, function } => {
                // Push a `(defining_class, self)` frame so a zero-arg
                // `super()` in the body can climb the MRO. The owning class
                // is found by walking the receiver instance's MRO for this
                // exact function; non-instance receivers (e.g. a classmethod
                // bound to the class object) skip the frame.
                let owner = match receiver.as_ref() {
                    Value::Instance(inst) => self.method_owner(&inst.class, &function),
                    _ => None,
                };
                if let Some(owner) = owner {
                    self.call_method_with_frame(&function, owner, *receiver, args, kwargs)
                } else {
                    let mut full_args = Vec::with_capacity(args.len() + 1);
                    full_args.push(*receiver);
                    full_args.extend(args);
                    self.call_function(&function, full_args, kwargs, None)
                }
            }
            Value::Class(c) => self.instantiate(&c, args, kwargs),
            // An instance is callable when its class defines `__call__`.
            Value::Instance(ref inst) => {
                if let Some(call) = self.find_method(&inst.class, "__call__") {
                    let owner = self
                        .method_owner(&inst.class, &call)
                        .unwrap_or_else(|| inst.class.clone());
                    self.call_method_with_frame(&call, owner, func.clone(), args, kwargs)
                } else {
                    Err(type_error(format!(
                        "'{}' object is not callable",
                        inst.class.name
                    )))
                }
            }
            other => Err(type_error(format!(
                "'{}' object is not callable",
                other.type_name()
            ))),
        }
    }

    /// Append a yielded value to the current generator's buffer, enforcing the
    /// eager-evaluation cap. Errors if no generator frame is active (a `yield`
    /// the detector missed) or the cap is exceeded.
    fn push_yield(&mut self, v: Value) -> Result<(), Unwind> {
        match self.gen_buffers.last_mut() {
            Some(buf) => {
                if buf.len() >= GENERATOR_CAP {
                    return Err(Unwind::Exception(VmException::new(
                        "RuntimeError",
                        format!(
                            "generator exceeded the VM's eager-evaluation limit ({} values); \
                             the tree-walking VM materialises generators eagerly — run unbounded \
                             or lazily-consumed generators with `tyc build` then `python`",
                            GENERATOR_CAP
                        ),
                    )));
                }
                buf.push(v);
                Ok(())
            }
            None => Err(Unwind::Exception(VmException::new(
                "RuntimeError",
                "`yield` outside of a generator function",
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
        let is_generator = body_is_generator(&f.body);
        let result = (|| -> Result<Value, Unwind> {
            self.bind_args(f, args, kwargs, receiver, &call_env)?;
            if is_generator {
                // Eager-collection generator: run the body to completion with
                // `yield`s captured into a fresh buffer, then hand back an
                // iterator over the collected values. `return` (with or without
                // a value) ends collection, matching a generator's StopIteration.
                self.gen_buffers.push(Vec::new());
                let outcome = self.exec_block(&f.body, &call_env);
                let buffer = self.gen_buffers.pop().unwrap_or_default();
                return match outcome {
                    Ok(()) | Err(Unwind::Return(_)) => {
                        Ok(Value::Iter(Rc::new(RefCell::new(IterState::List {
                            items: Rc::new(RefCell::new(buffer)),
                            index: 0,
                        }))))
                    }
                    Err(other) => Err(other),
                };
            }
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
        // Calling an enum class is value-lookup, not construction:
        // `Color(2)` → the member whose value is 2 (CPython semantics).
        if Self::is_enum_class(class) {
            return self.enum_lookup_by_value(class, args, kwargs);
        }
        let instance = Rc::new(Instance {
            class: class.clone(),
            fields: RefCell::new(HashMap::new()),
        });
        // Initialise class-level attributes that aren't methods. Skip the
        // internal enum sentinels so they don't leak onto instances.
        for (k, v) in class.class_attrs.borrow().iter() {
            if is_enum_sentinel(k) || k.starts_with("__typhon_setter__") {
                continue;
            }
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
        // Dataclass `__post_init__` hook — invoked right after field
        // initialisation when the constructor is auto-generated (matching
        // dataclasses, where the generated `__init__` calls it).
        if let Some(post) = self.find_method(class, "__post_init__") {
            let owner = self
                .method_owner(class, &post)
                .unwrap_or_else(|| class.clone());
            self.call_method_with_frame(
                &post,
                owner,
                Value::Instance(instance.clone()),
                vec![],
                &[],
            )?;
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

    /// Python truthiness honouring a user `__bool__` then `__len__` on
    /// instances (CPython's `object.__bool__` protocol). Falls back to the
    /// structural `Value::truthy` for everything else.
    pub fn is_truthy(&mut self, v: &Value) -> Result<bool, Unwind> {
        if let Value::Instance(i) = v {
            if self.find_method(&i.class, "__bool__").is_some() {
                if let Some(r) = self.call_dunder0(v, "__bool__")? {
                    return Ok(r.truthy());
                }
            }
            if self.find_method(&i.class, "__len__").is_some() {
                if let Some(r) = self.call_dunder0(v, "__len__")? {
                    return Ok(r.to_int()? != 0);
                }
            }
        }
        Ok(v.truthy())
    }

    /// `str(v)` honouring a user `__str__` (then `__repr__`) on instances.
    ///
    /// For containers, Python's `str()` renders elements via `repr()`, so a
    /// container delegates to `repr_of` (which recurses through user dunders
    /// on each element). Scalars keep their dedicated `__str__` path.
    pub fn str_of(&mut self, v: &Value) -> Result<String, Unwind> {
        if let Some(s) = self.enum_member_repr(v) {
            return Ok(s);
        }
        if Self::is_container(v) {
            return self.repr_of(v);
        }
        if let Some(r) = self.call_dunder0(v, "__str__")? {
            return require_str_return(r, "__str__");
        }
        if let Some(r) = self.call_dunder0(v, "__repr__")? {
            return require_str_return(r, "__repr__");
        }
        Ok(v.py_str())
    }

    /// `repr(v)` honouring a user `__repr__` on instances. Enum members
    /// flow through to `py_repr` → `instance_repr`, which renders the
    /// CPython `<Class.NAME: value>` form.
    pub fn repr_of(&mut self, v: &Value) -> Result<String, Unwind> {
        self.repr_of_depth(v, 0)
    }

    /// Whether a value is a container whose elements must be rendered through
    /// the interpreter so user `__repr__` / `__str__` dunders dispatch.
    fn is_container(v: &Value) -> bool {
        matches!(
            v,
            Value::List(_) | Value::Tuple(_) | Value::Dict(_) | Value::Set(_)
        )
    }

    /// `repr(v)` with a recursion-depth guard. Containers render each element
    /// via `repr_of` (so user `__repr__` dunders dispatch on elements), with
    /// EXACT CPython formatting replicated from `Value::py_str`. Scalars and
    /// every other `Value` kind delegate to the existing dunder / `py_repr`
    /// path unchanged. The depth cap falls back to `[...]` (CPython prints
    /// the same for direct self-reference) so self-referential containers
    /// don't blow the stack.
    fn repr_of_depth(&mut self, v: &Value, depth: usize) -> Result<String, Unwind> {
        const MAX_REPR_DEPTH: usize = 100;
        if depth >= MAX_REPR_DEPTH {
            return Ok("[...]".to_string());
        }
        match v {
            Value::List(l) => {
                // Clone the element handles out before recursing so the
                // RefCell borrow isn't held across `repr_of` calls (which
                // may themselves touch the same list, e.g. self-reference).
                let items: Vec<Value> = l.borrow().iter().cloned().collect();
                let mut s = String::from("[");
                for (i, elem) in items.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&self.repr_of_depth(elem, depth + 1)?);
                }
                s.push(']');
                Ok(s)
            }
            Value::Tuple(t) => {
                let items: Vec<Value> = t.iter().cloned().collect();
                let mut s = String::from("(");
                for (i, elem) in items.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&self.repr_of_depth(elem, depth + 1)?);
                }
                if items.len() == 1 {
                    s.push(',');
                }
                s.push(')');
                Ok(s)
            }
            Value::Dict(d) => {
                let frozen_key = HashKey::Str(Rc::new("__typhon_frozen__".to_owned()));
                let is_frozen = matches!(d.borrow().get(&frozen_key), Some(Value::Bool(true)));
                // Snapshot (key, value) pairs, filtering the synthetic
                // `__typhon_frozen__` sentinel, before recursing.
                let pairs: Vec<(HashKey, Value)> = d
                    .borrow()
                    .iter()
                    .filter(|(k, _)| {
                        !matches!(k, HashKey::Str(name) if name.as_str() == "__typhon_frozen__")
                    })
                    .map(|(k, val)| (k.clone(), val.clone()))
                    .collect();
                let mut s = String::new();
                if is_frozen {
                    s.push_str("mappingproxy({");
                } else {
                    s.push('{');
                }
                for (i, (k, val)) in pairs.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&self.repr_hashkey(k, depth + 1)?);
                    s.push_str(": ");
                    s.push_str(&self.repr_of_depth(val, depth + 1)?);
                }
                if is_frozen {
                    s.push_str("})");
                } else {
                    s.push('}');
                }
                Ok(s)
            }
            Value::Set(set) => {
                let frozen_key = HashKey::Str(Rc::new("__typhon_frozen__".to_owned()));
                let is_frozen = set.borrow().contains(&frozen_key);
                // Match `Value::py_str`'s ordering EXACTLY: sort by the
                // collision-safe `canonical_sort_key` so the repr is stable
                // and matches CPython for all-numeric / all-string cases.
                let mut keys: Vec<HashKey> = set
                    .borrow()
                    .iter()
                    .filter(|k| **k != frozen_key)
                    .cloned()
                    .collect();
                keys.sort_by_key(|k| k.canonical_sort_key());
                if keys.is_empty() {
                    return Ok(if is_frozen {
                        "frozenset()".to_string()
                    } else {
                        "set()".to_string()
                    });
                }
                let mut parts: Vec<String> = Vec::with_capacity(keys.len());
                for k in &keys {
                    parts.push(self.repr_hashkey(k, depth + 1)?);
                }
                let body = parts.join(", ");
                Ok(if is_frozen {
                    format!("frozenset({{{body}}})")
                } else {
                    format!("{{{body}}}")
                })
            }
            // Scalars, instances (incl. enum members → `instance_repr`),
            // Result Ok/Err, etc. keep the existing dunder / `py_repr` path.
            _ => {
                if let Some(r) = self.call_dunder0(v, "__repr__")? {
                    return require_str_return(r, "__repr__");
                }
                Ok(v.py_repr())
            }
        }
    }

    /// Render a `HashKey` (dict key / set element) to its CPython repr,
    /// recursing through `repr_of_depth` so user `__repr__` dunders dispatch
    /// on instance keys. `FrozenSet` keys keep the `frozenset({...})` wrapper
    /// that the plain `HashKey::into_value` round-trip would otherwise drop
    /// (it surfaces a frozenset as an untagged `Value::Set`).
    fn repr_hashkey(&mut self, k: &HashKey, depth: usize) -> Result<String, Unwind> {
        match k {
            HashKey::FrozenSet(items) => {
                if items.is_empty() {
                    return Ok("frozenset()".to_string());
                }
                // Match `Value::py_str` set ordering: sort by canonical key.
                let mut sorted: Vec<HashKey> = items.iter().cloned().collect();
                sorted.sort_by_key(|k| k.canonical_sort_key());
                let mut parts: Vec<String> = Vec::with_capacity(sorted.len());
                for inner in &sorted {
                    parts.push(self.repr_hashkey(inner, depth + 1)?);
                }
                Ok(format!("frozenset({{{}}})", parts.join(", ")))
            }
            other => self.repr_of_depth(&other.clone().into_value(), depth),
        }
    }

    /// `ClassName.MEMBER` rendering for an enum member instance, matching
    /// CPython's default `str(Color.RED)` / `repr` (both `Color.RED`).
    fn enum_member_repr(&self, v: &Value) -> Option<String> {
        if !Self::is_enum_member(v) {
            return None;
        }
        if let Value::Instance(i) = v {
            if let Some(Value::Str(name)) = i.fields.borrow().get("_name_") {
                return Some(format!("{}.{}", i.class.name, name));
            }
        }
        None
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
            (Float(a), Pow, Float(b)) => {
                // A negative base raised to a non-integer power is complex in
                // Python (`(-8) ** (1/3)` → ~`1+1.732j`), not `nan`.
                if *a < 0.0 && b.fract() != 0.0 {
                    let r = (-a).powf(*b);
                    let theta = std::f64::consts::PI * b;
                    return Ok(Complex(r * theta.cos(), r * theta.sin()));
                }
                return Ok(Float(a.powf(*b)));
            }
            // Complex base raised to a non-negative integer power — repeated
            // multiplication for an exact result (`(1j) ** 2` → `-1+0j`),
            // matching CPython's special-casing of integer exponents.
            (Complex(ar, ai), Pow, Int(b)) if !b.is_negative() => {
                let exp = b.to_u32().ok_or_else(overflow)?;
                let (mut rr, mut ri) = (1.0f64, 0.0f64);
                let (br, bi) = (*ar, *ai);
                for _ in 0..exp {
                    let nr = rr * br - ri * bi;
                    let ni = rr * bi + ri * br;
                    rr = nr;
                    ri = ni;
                }
                return Ok(Complex(rr, ri));
            }
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

        // Complex arithmetic. Any operand being a `Complex` promotes a numeric
        // (int / float / bool) other operand to `Complex(_, 0.0)`. Supports
        // `+ - * /` (CPython raises for `//`, `%`, `**`-with-complex in the
        // ways we don't need here — those fall through to the dunder/error
        // path, matching "unsupported" for the unsupported ops).
        if matches!(l, Complex(..)) || matches!(r, Complex(..)) {
            let lc = value_as_complex(l);
            let rc = value_as_complex(r);
            if let (Some((ar, ai)), Some((br, bi))) = (lc, rc) {
                return complex_binop(op, ar, ai, br, bi);
            }
            // One side is complex, the other isn't numeric — fall through to
            // the dunder/error path below.
        }

        // printf-style `%` formatting: `"%.2f and %s" % (3.14, "x")`. The
        // checker already accepts `str % args`; the VM implements the
        // common conversions. A single non-tuple value is treated as one
        // positional argument.
        if let (Str(fmt), Mod, _) = (l, op, r) {
            let values: Vec<Value> = match r {
                Tuple(t) => t.as_ref().clone(),
                other => vec![other.clone()],
            };
            return Ok(Str(Rc::new(printf_format(fmt, &values)?)));
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
            UnaryOp::Not => Ok(Value::Bool(!self.is_truthy(v)?)),
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
        // Honour a user `__index__` on the subscript key (e.g. `xs[idx]`
        // where `idx` is a class defining `__index__`).
        let key_owned;
        let key = match key {
            Value::Instance(i) if self.find_method(&i.class, "__index__").is_some() => {
                key_owned = self.call_dunder0(key, "__index__")?.unwrap_or(Value::None);
                &key_owned
            }
            _ => key,
        };
        match target {
            // `Color["RED"]` — enum member lookup by name.
            Value::Class(c) if Self::is_enum_class(c) => {
                let name = match key {
                    Value::Str(s) => s.as_str().to_owned(),
                    _ => {
                        return Err(key_error(key.py_repr()));
                    }
                };
                self.enum_lookup_by_name(c, &name)
            }
            Value::List(l) => {
                let i = key.to_int()?;
                let l = l.borrow();
                let idx = normalize_index(i, l.len())
                    .ok_or_else(|| index_error("list index out of range"))?;
                Ok(l[idx].clone())
            }
            // `range(...)[i]` — compute the i-th element arithmetically
            // (supports Python negative indexing).
            Value::Range { start, stop, step } => {
                let len = if *step > 0 {
                    ((stop - start).max(0) + step - 1) / step
                } else {
                    ((start - stop).max(0) - step - 1) / -step
                };
                let i = key.to_int()?;
                let idx = normalize_index(i, len as usize)
                    .ok_or_else(|| index_error("range object index out of range"))?;
                Ok(Value::Int(BigInt::from(start + idx as i64 * step)))
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
                // A plain `Value::Dict` has no associated class, so the
                // `__missing__` hook (mechanism #2) lives on the `Instance`
                // arm below — the builtins agent's `defaultdict` is an
                // Instance, not a bare Dict. A bare dict still raises KeyError.
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
                // Missing-key hook (mechanism #2). The builtins agent's
                // `defaultdict` is an `Instance` whose class defines
                // `__missing__(self, key)`. CPython's contract: when a key
                // lookup misses, call `__missing__(key)`, STORE the returned
                // value under `key`, and return it. We model storage by
                // dispatching the instance's own `__setitem__` (if any), then
                // returning the produced default.
                //
                // Dispatch order, matching CPython's `dict.__getitem__`:
                //   1. `__getitem__(key)` — if it succeeds, return it.
                //   2. If `__getitem__` raised `KeyError` (or is absent) and
                //      `__missing__` exists, call `__missing__(key)`, store via
                //      `__setitem__(key, value)`, and return `value`.
                let getitem = self.find_method(&i.class, "__getitem__");
                let missing = self.find_method(&i.class, "__missing__");
                if let Some(m) = getitem {
                    let res = self.call_value(
                        Value::BoundMethod {
                            receiver: Box::new(target.clone()),
                            function: m,
                        },
                        vec![key.clone()],
                        &[],
                    );
                    match res {
                        Ok(v) => return Ok(v),
                        Err(Unwind::Exception(ref e))
                            if missing.is_some() && e.kind == "KeyError" =>
                        {
                            // fall through to __missing__ below
                        }
                        Err(e) => return Err(e),
                    }
                }
                if let Some(m) = missing {
                    let value = self.call_value(
                        Value::BoundMethod {
                            receiver: Box::new(target.clone()),
                            function: m,
                        },
                        vec![key.clone()],
                        &[],
                    )?;
                    // Store the default under the key (CPython semantics).
                    if let Some(setitem) = self.find_method(&i.class, "__setitem__") {
                        self.call_value(
                            Value::BoundMethod {
                                receiver: Box::new(target.clone()),
                                function: setitem,
                            },
                            vec![key.clone(), value.clone()],
                            &[],
                        )?;
                    }
                    return Ok(value);
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
            // `del obj[key]` → `obj.__delitem__(key)`.
            Value::Instance(i) => {
                if let Some(m) = self.find_method(&i.class, "__delitem__") {
                    self.call_value(
                        Value::BoundMethod {
                            receiver: Box::new(target.clone()),
                            function: m,
                        },
                        vec![key.clone()],
                        &[],
                    )?;
                    return Ok(());
                }
                Err(type_error(format!(
                    "'{}' object does not support item deletion",
                    i.class.name
                )))
            }
            _ => Err(type_error("delete on unsupported target")),
        }
    }

    pub fn get_attr(&mut self, value: &Value, attr: &str) -> Result<Value, Unwind> {
        // `complex` exposes `.real` / `.imag` / `.conjugate()` (FINDINGS: complex
        // ctor shim). Components are floats, matching CPython.
        if let Value::Complex(re, im) = value {
            match attr {
                "real" => return Ok(Value::Float(*re)),
                "imag" => return Ok(Value::Float(*im)),
                "conjugate" => {
                    let (re, im) = (*re, *im);
                    return Ok(Value::Native(Rc::new(NativeFn::new(
                        "conjugate",
                        move |_i, _args| Ok(Value::Complex(re, -im)),
                    ))));
                }
                _ => {}
            }
        }
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
                // Pydantic `model` instances expose `model_dump()` →
                // dict-of-fields and `model_dump_json()` → JSON string.
                if attr == "model_dump" || attr == "model_dump_json" {
                    let inst = inst.clone();
                    let as_json = attr == "model_dump_json";
                    let nf = NativeFn::new("model_dump", move |_i, _args| {
                        let mut map: DictMap = IndexMap::new();
                        let fields = inst.fields.borrow();
                        for field in &inst.class.fields {
                            if let Some(v) = fields.get(&field.name) {
                                map.insert(HashKey::Str(Rc::new(field.name.clone())), v.clone());
                            }
                        }
                        let dict = Value::Dict(Rc::new(RefCell::new(map)));
                        if as_json {
                            Ok(Value::Str(Rc::new(crate::builtins::json_dumps_pub(&dict))))
                        } else {
                            Ok(dict)
                        }
                    });
                    return Ok(Value::Native(Rc::new(nf)));
                }
                // Fall back to class-level attributes (ClassVar / plain-class
                // constants) — `instance.K` reads `type(instance).K`.
                if let Some(v) = inst.class.class_attrs.borrow().get(attr) {
                    if !is_enum_sentinel(attr) {
                        return Ok(v.clone());
                    }
                }
                // Last resort: a user `__getattr__(self, name)` resolves
                // otherwise-missing attributes (CPython protocol).
                if attr != "__getattr__" {
                    if let Some(m) = self.find_method(&inst.class, "__getattr__") {
                        return self.call_value(
                            Value::BoundMethod {
                                receiver: Box::new(value.clone()),
                                function: m,
                            },
                            vec![Value::Str(Rc::new(attr.to_owned()))],
                            &[],
                        );
                    }
                }
                Err(attribute_error(format!(
                    "'{}' object has no attribute '{}'",
                    inst.class.name, attr
                )))
            }
            Value::Class(class) => {
                // `Cls.__name__` / `type(x).__name__`.
                if attr == "__name__" || attr == "__qualname__" {
                    return Ok(Value::Str(Rc::new(class.name.clone())));
                }
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
                // Pydantic `model` class methods: `Model.model_validate(dict)`
                // builds an instance from a mapping (validation is not modelled;
                // it maps fields to constructor kwargs).
                if attr == "model_validate" {
                    let cls = class.clone();
                    let nf = NativeFn::new("model_validate", move |interp, args| {
                        let arg = args
                            .into_iter()
                            .next()
                            .ok_or_else(|| type_error("model_validate() requires a mapping"))?;
                        let Value::Dict(d) = arg else {
                            return Err(type_error("model_validate() expects a dict"));
                        };
                        let kwargs: Vec<(String, Value)> = d
                            .borrow()
                            .iter()
                            .filter_map(|(k, v)| match k {
                                HashKey::Str(s) => Some(((**s).clone(), v.clone())),
                                _ => None,
                            })
                            .collect();
                        interp.instantiate(&cls, vec![], &kwargs)
                    });
                    return Ok(Value::Native(Rc::new(nf)));
                }
                Err(attribute_error(format!(
                    "type object '{}' has no attribute '{}'",
                    class.name, attr
                )))
            }
            Value::Module(m) => m.members.borrow().get(attr).cloned().ok_or_else(|| {
                attribute_error(format!("module '{}' has no attribute '{}'", m.name, attr))
            }),
            // `func.__name__` / `func.__qualname__`.
            Value::Function(f) if attr == "__name__" || attr == "__qualname__" => {
                Ok(Value::Str(Rc::new(f.name.clone())))
            }
            Value::Native(n) if attr == "__name__" || attr == "__qualname__" => {
                Ok(Value::Str(Rc::new(n.name.to_string())))
            }
            Value::BoundMethod { function, .. } if attr == "__name__" || attr == "__qualname__" => {
                Ok(Value::Str(Rc::new(function.name.clone())))
            }
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
                // H5b — accessing an unknown (non-method, non-dunder)
                // attribute on a built-in value must raise `AttributeError`
                // rather than returning a bogus bound-method object.
                if !builtin_has_attr(value, attr) {
                    return Err(attribute_error(format!(
                        "'{}' object has no attribute '{}'",
                        value.type_name(),
                        attr
                    )));
                }
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
            Value::Exception {
                kind,
                message,
                args,
            } => match attr {
                "args" => {
                    if args.is_empty() && !message.is_empty() {
                        Ok(Value::Tuple(Rc::new(vec![Value::Str(message.clone())])))
                    } else {
                        Ok(Value::Tuple(args.clone()))
                    }
                }
                "kind" => Ok(Value::Str(kind.clone())),
                // `__cause__` / `__context__` are not tracked yet; expose None
                // so the common introspection (`e.__cause__ is None`) works.
                "__cause__" | "__context__" => Ok(Value::None),
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

    pub fn set_attr(&mut self, receiver: &Value, attr: &str, value: Value) -> Result<(), Unwind> {
        match receiver {
            Value::Instance(i) => {
                // A `@prop.setter` registered for this name intercepts the
                // assignment (`obj.prop = v` → setter(self, v)).
                let setter = i
                    .class
                    .class_attrs
                    .borrow()
                    .get(&format!("__typhon_setter__{}", attr))
                    .cloned();
                if let Some(Value::Function(setter)) = setter {
                    self.call_value(
                        Value::BoundMethod {
                            receiver: Box::new(receiver.clone()),
                            function: setter,
                        },
                        vec![value],
                        &[],
                    )?;
                    return Ok(());
                }
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
        // Slice assignment: `xs[1:3] = [...]`.
        if let Value::Tuple(t) = key {
            if t.len() == 4 {
                if let Value::Str(tag) = &t[0] {
                    if tag.as_str() == "__slice__" {
                        return self.set_slice(target, &t[1], &t[2], &t[3], value);
                    }
                }
            }
        }
        // Honour a user `__index__` on the subscript key.
        let key_owned;
        let key = match key {
            Value::Instance(i) if self.find_method(&i.class, "__index__").is_some() => {
                key_owned = self.call_dunder0(key, "__index__")?.unwrap_or(Value::None);
                &key_owned
            }
            _ => key,
        };
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
            // `obj[key] = value` → `obj.__setitem__(key, value)` when the
            // instance's class defines it. Needed by the builtins agent's
            // `defaultdict` (and any user mapping type) so subscript-assignment
            // round-trips with the `__missing__` read path (mechanism #2).
            Value::Instance(i) => {
                if let Some(m) = self.find_method(&i.class, "__setitem__") {
                    self.call_value(
                        Value::BoundMethod {
                            receiver: Box::new(target.clone()),
                            function: m,
                        },
                        vec![key.clone(), value],
                        &[],
                    )?;
                    return Ok(());
                }
                Err(type_error(format!(
                    "'{}' object does not support item assignment",
                    target.type_name()
                )))
            }
            other => Err(type_error(format!(
                "'{}' object does not support item assignment",
                other.type_name()
            ))),
        }
    }

    /// `xs[a:b:c] = iterable` — list slice assignment.
    fn set_slice(
        &mut self,
        target: &Value,
        lower: &Value,
        upper: &Value,
        step: &Value,
        value: Value,
    ) -> Result<(), Unwind> {
        let Value::List(l) = target else {
            return Err(type_error(format!(
                "'{}' object does not support slice assignment",
                target.type_name()
            )));
        };
        let len = l.borrow().len();
        let step_i = match step {
            Value::None => 1,
            v => v.to_int()?,
        };
        if step_i == 0 {
            return Err(value_error("slice step cannot be zero"));
        }
        // Materialise the RHS.
        let it = self.make_iter(value)?;
        let mut repl: Vec<Value> = Vec::new();
        while let Some(v) = self.iter_next(&it)? {
            repl.push(v);
        }
        let (start, stop, step_i) = compute_slice(lower, upper, step_i, len)?;
        if step_i == 1 {
            let s = start.max(0) as usize;
            let e = (stop.max(0) as usize).clamp(s, len);
            l.borrow_mut().splice(s..e, repl);
            Ok(())
        } else {
            // Extended slice: the replacement length must match exactly.
            let mut indices: Vec<usize> = Vec::new();
            let mut idx = start;
            if step_i > 0 {
                while idx < stop {
                    if idx >= 0 {
                        indices.push(idx as usize);
                    }
                    idx += step_i;
                }
            } else {
                while idx > stop {
                    if idx >= 0 {
                        indices.push(idx as usize);
                    }
                    idx += step_i;
                }
            }
            if indices.len() != repl.len() {
                return Err(value_error(format!(
                    "attempt to assign sequence of size {} to extended slice of size {}",
                    repl.len(),
                    indices.len()
                )));
            }
            let mut b = l.borrow_mut();
            for (i, v) in indices.into_iter().zip(repl) {
                if i < b.len() {
                    b[i] = v;
                }
            }
            Ok(())
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
            // Iterating bytes/bytearray yields each byte as an int
            // (`list(b"\x01\x02")` → `[1, 2]`).
            Value::Bytes(b) => {
                let items: Vec<Value> = b
                    .iter()
                    .map(|byte| Value::Int(BigInt::from(*byte)))
                    .collect();
                IterState::List {
                    items: Rc::new(RefCell::new(items)),
                    index: 0,
                }
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
            // A dict-view iterates over its materialised items, so
            // `for k in d.keys()`, `list(d.values())`, `set(d.items())` work.
            Value::DictView { items, .. } => IterState::List {
                items: Rc::new(RefCell::new(items)),
                index: 0,
            },
            // Iterating an enum class yields its members in definition order.
            Value::Class(ref c) if Self::is_enum_class(c) => {
                let members = match c.class_attrs.borrow().get("__typhon_enum_members__") {
                    Some(Value::List(l)) => l.borrow().clone(),
                    _ => Vec::new(),
                };
                IterState::List {
                    items: Rc::new(RefCell::new(members)),
                    index: 0,
                }
            }
            // A user/synthesised instance defining `__iter__` delegates to it
            // (used by the `defaultdict` shim so `for k in dd` / `list(dd)`
            // iterate the backing mapping's keys).
            Value::Instance(ref inst) => {
                if let Some(m) = self.find_method(&inst.class, "__iter__") {
                    let iter_val = self.call_value(
                        Value::BoundMethod {
                            receiver: Box::new(v.clone()),
                            function: m,
                        },
                        vec![],
                        &[],
                    )?;
                    // If `__iter__` returns an object that drives iteration via
                    // `__next__` (commonly `return self`), step it eagerly via
                    // `__next__` rather than recursing into `make_iter` — which
                    // would loop forever for `return self` (FINDINGS G4).
                    if let Value::Instance(ret) = &iter_val {
                        if self.find_method(&ret.class, "__next__").is_some() {
                            let mut items: Vec<Value> = Vec::new();
                            loop {
                                if items.len() >= GENERATOR_CAP {
                                    return Err(Unwind::Exception(VmException::new(
                                        "RuntimeError",
                                        "iterator exceeded the VM's eager-evaluation limit",
                                    )));
                                }
                                match self.call_dunder0(&iter_val, "__next__") {
                                    Ok(Some(item)) => items.push(item),
                                    Ok(None) => break,
                                    Err(Unwind::Exception(e)) if e.kind == "StopIteration" => break,
                                    Err(e) => return Err(e),
                                }
                            }
                            return Ok(Value::Iter(Rc::new(RefCell::new(IterState::List {
                                items: Rc::new(RefCell::new(items)),
                                index: 0,
                            }))));
                        }
                    }
                    return self.make_iter(iter_val);
                }
                return Err(type_error(format!(
                    "'{}' object is not iterable",
                    v.type_name()
                )));
            }
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
                        let kv = self.call_value(func.clone(), vec![v.clone()], &[])?;
                        let keep = self.is_truthy(&kv)?;
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
        self.run_comprehension_leaking(generators, &[], env, &mut emit)
    }

    /// Like `run_comprehension`, but after the comprehension finishes, copies
    /// any walrus (`:=`) target named in `leak_names` from the comprehension's
    /// private scope out into the enclosing `env` — Python leaks the LAST
    /// value of a comprehension walrus target into the containing scope.
    fn run_comprehension_leaking<F>(
        &mut self,
        generators: &[ast::Comprehension],
        leak_names: &[String],
        env: &EnvRef,
        emit: &mut F,
    ) -> Result<(), Unwind>
    where
        F: FnMut(&mut Self, &EnvRef) -> Result<(), Unwind>,
    {
        let scope = Env::new_child(env);
        let res = self.run_comp_recurse(generators, 0, &scope, emit);
        for name in leak_names {
            if let Some(v) = scope.get(name) {
                env.assign_or_create(name, v);
            }
        }
        res
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
                let __ac = self.eval_expr(cond, env)?;
                if !self.is_truthy(&__ac)? {
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
        let leaks = comprehension_walrus_names(&[&c.elt], &c.generators);
        self.run_comprehension_leaking(&c.generators, &leaks, env, &mut move |this, scope| {
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
        let leaks = comprehension_walrus_names(&[&c.elt], &c.generators);
        self.run_comprehension_leaking(&c.generators, &leaks, env, &mut move |this, scope| {
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
        let mut parts: Vec<&Expr> = vec![&c.value];
        if let Some(k) = c.key.as_deref() {
            parts.push(k);
        }
        let leaks = comprehension_walrus_names(&parts, &c.generators);
        self.run_comprehension_leaking(&c.generators, &leaks, env, &mut move |this, scope| {
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
                            Some(v @ (Value::Instance(_) | Value::Exception { .. })) => v.clone(),
                            _ => Value::Exception {
                                kind: Rc::new(exc.kind.clone()),
                                message: Rc::new(exc.message.clone()),
                                args: Rc::new(exc_fallback_args(&exc.message)),
                            },
                        };
                        if let Some(name) = &h.name {
                            env.set(name.as_str(), value.clone());
                        }
                        handled_exc = Some(value);
                        // Make this exception the active one so a bare `raise`
                        // inside the handler re-raises it.
                        self.active_exceptions.push(exc.clone());
                        let result = self.exec_block(&h.body, env);
                        self.active_exceptions.pop();
                        if let Some(name) = &h.name {
                            env.delete(name.as_str());
                        }
                        if let Err(e) = result {
                            // An error escaping the handler: finally still runs,
                            // and if finally itself raises, that wins (D5).
                            if let Err(fin) = self.exec_block(&t.finalbody, env) {
                                return Err(fin);
                            }
                            return Err(e);
                        }
                        break;
                    }
                }
                if !found {
                    if let Err(fin) = self.exec_block(&t.finalbody, env) {
                        return Err(fin);
                    }
                    return Err(Unwind::Exception(exc));
                }
                Ok(())
            }
            Err(other) => {
                if let Err(fin) = self.exec_block(&t.finalbody, env) {
                    return Err(fin);
                }
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
            // Direct name match, or a builtin-exception-hierarchy match
            // (e.g. `except ArithmeticError` catching `ZeroDivisionError`,
            // `except LookupError` catching `KeyError`/`IndexError`,
            // `except OSError` catching `FileNotFoundError`).
            if name == exc.kind || builtin_exc_is_a(&exc.kind, name) {
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
            Value::Exception {
                ref kind,
                ref message,
                ..
            } => {
                // Keep the full value (carrying `args`) attached so the
                // handler can bind it and `e.args` survives.
                let (k, m) = ((**kind).clone(), (**message).clone());
                Unwind::Exception(VmException::new(k, m).with_value(v))
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
            // A `@contextmanager`-decorated generator can't act as a context
            // manager under eager evaluation: the VM runs the whole generator
            // body (setup *and* teardown) at call time, so there's no point at
            // which to run the `with` body between them. Surface a clear hint
            // rather than a cryptic missing-`__enter__` AttributeError.
            if matches!(cm, Value::Iter(_)) {
                return Err(vm_unsupported_use_compile(
                    "`@contextmanager` generators as context managers (the VM evaluates \
                     generators eagerly); run with `tyc build` then `python`",
                ));
            }
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
        let mut body_res = self.exec_block(&w.body, env);
        // Call __exit__ on each, in reverse order. When the body raised, pass
        // the exception info `(exc_type, exc_value, None)` and honour a truthy
        // return value by SUPPRESSING the exception (CPython protocol).
        for cm in entered.into_iter().rev() {
            if let Ok(exit) = self.get_attr(&cm, "__exit__") {
                let (et, ev) = match &body_res {
                    Err(Unwind::Exception(exc)) => {
                        let value = match &exc.value {
                            Some(v) => v.clone(),
                            None => Value::Exception {
                                kind: Rc::new(exc.kind.clone()),
                                message: Rc::new(exc.message.clone()),
                                args: Rc::new(exc_fallback_args(&exc.message)),
                            },
                        };
                        let etype = match &exc.value {
                            Some(Value::Instance(i)) => Value::Class(i.class.clone()),
                            _ => crate::builtins::make_builtin_type(&exc.kind),
                        };
                        (etype, value)
                    }
                    _ => (Value::None, Value::None),
                };
                let raised = matches!(&body_res, Err(Unwind::Exception(_)));
                let suppressed = self.call_value(exit, vec![et, ev, Value::None], &[])?;
                if raised && self.is_truthy(&suppressed)? {
                    body_res = Ok(());
                }
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
                    Some(g) => {
                        let gv = self.eval_expr(g, &scope)?;
                        self.is_truthy(&gv)?
                    }
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
        // Imaginary literal, e.g. `2j` → `Complex(0.0, 2.0)`, `3+4j` is parsed
        // as `Int(3) + Complex(0.0, 4.0)` so only the `imag` part is set here.
        Number::Complex { real, imag } => Value::Complex(*real, *imag),
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

/// Internal enum bookkeeping class-attr names that must not be inherited by
/// subclasses or leak onto instances.
fn is_enum_sentinel(name: &str) -> bool {
    matches!(name, "__typhon_enum_base__" | "__typhon_enum_members__")
}

/// Whether a built-in value actually has the named attribute/method. Mirrors
/// the method tables in `builtins.rs` (which this crate may not edit). Any
/// dunder name passes through so internal probing paths keep working; only
/// plain unknown names are rejected, which is what makes `(5).foo` raise
/// `AttributeError` (H5b) instead of returning a bogus bound method.
fn builtin_has_attr(value: &Value, attr: &str) -> bool {
    if attr.starts_with("__") && attr.ends_with("__") {
        return true;
    }
    match value {
        Value::Str(_) => matches!(
            attr,
            "capitalize"
                | "casefold"
                | "center"
                | "count"
                | "encode"
                | "endswith"
                | "expandtabs"
                | "find"
                | "format"
                | "index"
                | "isalnum"
                | "isalpha"
                | "isdecimal"
                | "isdigit"
                | "islower"
                | "isnumeric"
                | "isspace"
                | "istitle"
                | "isupper"
                | "join"
                | "ljust"
                | "lower"
                | "lstrip"
                | "partition"
                | "removeprefix"
                | "removesuffix"
                | "replace"
                | "rfind"
                | "rindex"
                | "rjust"
                | "rpartition"
                | "rsplit"
                | "rstrip"
                | "split"
                | "splitlines"
                | "startswith"
                | "strip"
                | "swapcase"
                | "title"
                | "upper"
                | "zfill"
        ),
        Value::Bytes(_) => matches!(
            attr,
            "decode"
                | "hex"
                | "lower"
                | "upper"
                | "split"
                | "rsplit"
                | "strip"
                | "lstrip"
                | "rstrip"
                | "startswith"
                | "endswith"
                | "find"
                | "index"
                | "count"
                | "replace"
                | "join"
        ),
        Value::List(_) => matches!(
            attr,
            "append"
                | "appendleft"
                | "clear"
                | "copy"
                | "count"
                | "extend"
                | "extendleft"
                | "index"
                | "insert"
                | "pop"
                | "popleft"
                | "remove"
                | "reverse"
                | "rotate"
                | "sort"
        ),
        Value::Dict(_) => matches!(
            attr,
            "clear"
                | "copy"
                | "elements"
                | "get"
                | "items"
                | "keys"
                | "most_common"
                | "pop"
                | "popitem"
                | "setdefault"
                | "update"
                | "values"
        ),
        Value::Set(_) => matches!(
            attr,
            "add"
                | "clear"
                | "copy"
                | "difference"
                | "discard"
                | "intersection"
                | "isdisjoint"
                | "issubset"
                | "issuperset"
                | "pop"
                | "remove"
                | "symmetric_difference"
                | "union"
                | "update"
        ),
        Value::Tuple(_) => matches!(attr, "count" | "index"),
        Value::Float(_) => matches!(attr, "is_integer"),
        Value::Int(_) | Value::Bool(_) => {
            matches!(attr, "bit_length" | "bit_count" | "to_bytes" | "is_integer")
        }
        _ => false,
    }
}

/// printf-style `%` string formatting (`"%.2f and %s" % (3.14, "x")`).
/// Supports `%s %r %d %i %f %e %E %g %G %x %X %o %c %%` with optional
/// flags (`-`, `+`, ` `, `0`, `#`), width, and `.precision`. Enough to
/// cover the common cases; matches CPython for those conversions.
fn printf_format(fmt: &str, values: &[Value]) -> Result<String, Unwind> {
    let chars: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;
    let mut arg = 0usize;
    let next_arg = |arg: &mut usize| -> Result<&Value, Unwind> {
        let v = values
            .get(*arg)
            .ok_or_else(|| type_error("not enough arguments for format string"))?;
        *arg += 1;
        Ok(v)
    };
    while i < chars.len() {
        if chars[i] != '%' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        i += 1; // consume '%'
        if i >= chars.len() {
            return Err(value_error("incomplete format"));
        }
        if chars[i] == '%' {
            out.push('%');
            i += 1;
            continue;
        }
        // Flags.
        let mut flag_minus = false;
        let mut flag_plus = false;
        let mut flag_space = false;
        let mut flag_zero = false;
        let mut flag_alt = false;
        loop {
            match chars.get(i) {
                Some('-') => flag_minus = true,
                Some('+') => flag_plus = true,
                Some(' ') => flag_space = true,
                Some('0') => flag_zero = true,
                Some('#') => flag_alt = true,
                _ => break,
            }
            i += 1;
        }
        // Width.
        let mut width: Option<usize> = None;
        while let Some(c) = chars.get(i) {
            if let Some(d) = c.to_digit(10) {
                width = Some(width.unwrap_or(0) * 10 + d as usize);
                i += 1;
            } else {
                break;
            }
        }
        // Precision.
        let mut precision: Option<usize> = None;
        if chars.get(i) == Some(&'.') {
            i += 1;
            let mut p = 0usize;
            while let Some(c) = chars.get(i) {
                if let Some(d) = c.to_digit(10) {
                    p = p * 10 + d as usize;
                    i += 1;
                } else {
                    break;
                }
            }
            precision = Some(p);
        }
        let conv = *chars
            .get(i)
            .ok_or_else(|| value_error("incomplete format"))?;
        i += 1;

        let body: String = match conv {
            's' => {
                let v = next_arg(&mut arg)?;
                let mut s = v.py_str();
                if let Some(p) = precision {
                    s = s.chars().take(p).collect();
                }
                s
            }
            'r' => {
                let v = next_arg(&mut arg)?;
                let mut s = v.py_repr();
                if let Some(p) = precision {
                    s = s.chars().take(p).collect();
                }
                s
            }
            'c' => {
                let v = next_arg(&mut arg)?;
                match v {
                    Value::Str(s) => s.chars().next().map(String::from).unwrap_or_default(),
                    other => {
                        let n = other.to_int()?;
                        char::from_u32(n as u32)
                            .map(String::from)
                            .unwrap_or_default()
                    }
                }
            }
            'd' | 'i' => {
                let v = next_arg(&mut arg)?;
                let iv = v.to_bigint()?;
                printf_signed(
                    &iv.abs().to_str_radix(10),
                    iv.is_negative(),
                    flag_plus,
                    flag_space,
                )
            }
            'x' | 'X' | 'o' => {
                let v = next_arg(&mut arg)?;
                let iv = v.to_bigint()?;
                let neg = iv.is_negative();
                let abs = iv.abs();
                let mut digits = match conv {
                    'x' => abs.to_str_radix(16),
                    'X' => abs.to_str_radix(16).to_uppercase(),
                    _ => abs.to_str_radix(8),
                };
                if flag_alt {
                    let prefix = match conv {
                        'x' => "0x",
                        'X' => "0X",
                        _ => "0o",
                    };
                    digits = format!("{prefix}{digits}");
                }
                printf_signed(&digits, neg, flag_plus, flag_space)
            }
            'f' | 'F' | 'e' | 'E' | 'g' | 'G' => {
                let v = next_arg(&mut arg)?;
                let x = v.to_float()?;
                let p = precision.unwrap_or(6);
                let neg = x.is_sign_negative() && x != 0.0;
                let abs = x.abs();
                let digits = match conv {
                    'e' => normalise_exp_notation(format!("{:.*e}", p, abs), false),
                    'E' => normalise_exp_notation(format!("{:.*e}", p, abs), true),
                    'g' | 'G' => format_g(abs, if p == 0 { 1 } else { p }, conv == 'G'),
                    _ => format!("{:.*}", p, abs),
                };
                printf_signed(&digits, neg, flag_plus, flag_space)
            }
            other => {
                return Err(value_error(format!(
                    "unsupported format character '{other}'"
                )))
            }
        };

        out.push_str(&pad_printf(&body, width, flag_minus, flag_zero, conv));
    }
    Ok(out)
}

fn printf_signed(magnitude: &str, neg: bool, plus: bool, space: bool) -> String {
    if neg {
        format!("-{magnitude}")
    } else if plus {
        format!("+{magnitude}")
    } else if space {
        format!(" {magnitude}")
    } else {
        magnitude.to_owned()
    }
}

fn pad_printf(body: &str, width: Option<usize>, left: bool, zero: bool, conv: char) -> String {
    let Some(w) = width else {
        return body.to_owned();
    };
    let len = body.chars().count();
    if len >= w {
        return body.to_owned();
    }
    let pad = w - len;
    if left {
        let p: String = std::iter::repeat_n(' ', pad).collect();
        format!("{body}{p}")
    } else if zero
        && matches!(
            conv,
            'd' | 'i' | 'f' | 'F' | 'e' | 'E' | 'g' | 'G' | 'x' | 'X' | 'o'
        )
    {
        // Zero-pad after an optional sign.
        let (sign, rest) = match body.strip_prefix(['-', '+', ' ']) {
            Some(r) => (&body[..1], r),
            None => ("", body),
        };
        let p: String = std::iter::repeat_n('0', pad).collect();
        format!("{sign}{p}{rest}")
    } else {
        let p: String = std::iter::repeat_n(' ', pad).collect();
        format!("{p}{body}")
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

/// Whether a function body is a generator — i.e. contains a `yield` /
/// `yield from` reachable without crossing a nested function/lambda boundary.
fn body_is_generator(body: &[Stmt]) -> bool {
    body.iter().any(stmt_has_yield)
}

fn stmt_has_yield(s: &Stmt) -> bool {
    use ruff_python_ast::Stmt::*;
    match s {
        // Nested function / class scopes own their own yields.
        FunctionDef(_) | ClassDef(_) => false,
        Expr(e) => expr_has_yield(&e.value),
        Return(r) => r.value.as_deref().is_some_and(expr_has_yield),
        Assign(a) => expr_has_yield(&a.value),
        AugAssign(a) => expr_has_yield(&a.value),
        AnnAssign(a) => a.value.as_deref().is_some_and(expr_has_yield),
        If(x) => {
            expr_has_yield(&x.test)
                || body_is_generator(&x.body)
                || x.elif_else_clauses
                    .iter()
                    .any(|c| body_is_generator(&c.body))
        }
        While(x) => {
            expr_has_yield(&x.test) || body_is_generator(&x.body) || body_is_generator(&x.orelse)
        }
        For(x) => {
            expr_has_yield(&x.iter) || body_is_generator(&x.body) || body_is_generator(&x.orelse)
        }
        With(x) => {
            x.items.iter().any(|i| expr_has_yield(&i.context_expr)) || body_is_generator(&x.body)
        }
        Match(x) => {
            expr_has_yield(&x.subject) || x.cases.iter().any(|c| body_is_generator(&c.body))
        }
        Try(x) => {
            body_is_generator(&x.body)
                || x.handlers.iter().any(|h| {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    body_is_generator(&h.body)
                })
                || body_is_generator(&x.orelse)
                || body_is_generator(&x.finalbody)
        }
        _ => false,
    }
}

fn expr_has_yield(e: &Expr) -> bool {
    use ruff_python_ast::Expr::*;
    match e {
        Yield(_) | YieldFrom(_) => true,
        // A lambda is its own scope — its (rare) yields aren't ours.
        Lambda(_) => false,
        BoolOp(x) => x.values.iter().any(expr_has_yield),
        BinOp(x) => expr_has_yield(&x.left) || expr_has_yield(&x.right),
        UnaryOp(x) => expr_has_yield(&x.operand),
        Compare(x) => expr_has_yield(&x.left) || x.comparators.iter().any(expr_has_yield),
        Call(x) => {
            expr_has_yield(&x.func)
                || x.arguments.args.iter().any(expr_has_yield)
                || x.arguments
                    .keywords
                    .iter()
                    .any(|k| expr_has_yield(&k.value))
        }
        Tuple(x) => x.elts.iter().any(expr_has_yield),
        List(x) => x.elts.iter().any(expr_has_yield),
        Set(x) => x.elts.iter().any(expr_has_yield),
        Starred(x) => expr_has_yield(&x.value),
        If(x) => expr_has_yield(&x.test) || expr_has_yield(&x.body) || expr_has_yield(&x.orelse),
        Named(x) => expr_has_yield(&x.value),
        Await(x) => expr_has_yield(&x.value),
        Subscript(x) => expr_has_yield(&x.value) || expr_has_yield(&x.slice),
        Attribute(x) => expr_has_yield(&x.value),
        // Comprehensions: only the outermost iterable runs in the enclosing
        // scope, so a `yield` there belongs to us. (Yields elsewhere in a
        // comprehension are a SyntaxError, so scanning them is harmless.)
        ListComp(x) => comprehension_has_yield(&x.generators) || expr_has_yield(&x.elt),
        SetComp(x) => comprehension_has_yield(&x.generators) || expr_has_yield(&x.elt),
        Generator(x) => comprehension_has_yield(&x.generators) || expr_has_yield(&x.elt),
        DictComp(x) => {
            comprehension_has_yield(&x.generators)
                || x.key.as_deref().is_some_and(expr_has_yield)
                || expr_has_yield(&x.value)
        }
        FString(x) => fstring_has_yield(x),
        _ => false,
    }
}

fn comprehension_has_yield(generators: &[ast::Comprehension]) -> bool {
    generators
        .iter()
        .any(|g| expr_has_yield(&g.iter) || g.ifs.iter().any(expr_has_yield))
}

fn fstring_has_yield(f: &ast::ExprFString) -> bool {
    f.value.iter().any(|part| match part {
        FStringPart::Literal(_) => false,
        FStringPart::FString(fs) => fs.elements.iter().any(|el| match el {
            InterpolatedStringElement::Literal(_) => false,
            InterpolatedStringElement::Interpolation(interp) => expr_has_yield(&interp.expression),
        }),
    })
}

/// Enforce that `__str__` / `__repr__` returned a `str` (CPython raises
/// `TypeError` otherwise) and unwrap it.
fn require_str_return(v: Value, dunder: &str) -> Result<String, Unwind> {
    match v {
        Value::Str(s) => Ok((*s).clone()),
        other => Err(Unwind::Exception(VmException::new(
            "TypeError",
            format!(
                "{} returned non-string (type {})",
                dunder,
                other.type_name()
            ),
        ))),
    }
}

/// The dunder method name a binary operator dispatches to on its left operand.
/// View a numeric `Value` as a complex `(real, imag)` pair, or `None` if it
/// isn't a number. `int` / `float` / `bool` map to a zero imaginary part.
fn value_as_complex(v: &Value) -> Option<(f64, f64)> {
    match v {
        Value::Complex(re, im) => Some((*re, *im)),
        Value::Float(x) => Some((*x, 0.0)),
        Value::Int(i) => Some((bigint_to_f64(i), 0.0)),
        Value::Bool(b) => Some((*b as i64 as f64, 0.0)),
        _ => None,
    }
}

/// Complex arithmetic for `+ - * /`. Other operators return the standard
/// "unsupported operand" error.
fn complex_binop(op: Operator, ar: f64, ai: f64, br: f64, bi: f64) -> Result<Value, Unwind> {
    use Operator::*;
    match op {
        Add => Ok(Value::Complex(ar + br, ai + bi)),
        Sub => Ok(Value::Complex(ar - br, ai - bi)),
        Mult => Ok(Value::Complex(ar * br - ai * bi, ar * bi + ai * br)),
        Div => {
            // (a / b) = (a * conj(b)) / |b|^2
            let denom = br * br + bi * bi;
            if denom == 0.0 {
                return Err(zero_division());
            }
            Ok(Value::Complex(
                (ar * br + ai * bi) / denom,
                (ai * br - ar * bi) / denom,
            ))
        }
        _ => Err(type_error(format!(
            "unsupported operand type(s) for {}: 'complex' and 'complex'",
            op.as_str()
        ))),
    }
}

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

/// Build a bound `NativeFn` that implements one of the four Result
/// combinators (`map`, `map_err`, `and_then`, `or_else`) over a captured
/// receiver. The four follow Rust's `Result` semantics:
///
/// * `Ok.map(f)` → `Ok(f(value))`; `Err.map(_)` → identity.
/// * `Ok.map_err(_)` → identity; `Err.map_err(g)` → `Err(g(error))`.
/// * `Ok.and_then(h)` → `h(value)` (which itself must return a `Result`);
///   `Err.and_then(_)` → identity.
/// * `Ok.or_else(_)` → identity; `Err.or_else(k)` → `k(error)`.
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

/// Whether builtin exception `kind` is `target` or one of its subclasses in
/// the standard CPython exception hierarchy. `Exception` / `BaseException`
/// match everything except the two bare base-only kinds handled by the
/// caller. Returns false for unknown names (user exceptions go through the
/// instance-MRO path instead).
/// The `pub` names a module file (or a sub-package's `__init__.ty`) exports,
/// read via the preprocessor. Returns an empty vec for a file that has no
/// `pub` declarations (e.g. a sub-package whose `__init__.ty` is `pub *`).
fn read_pub_names(path: &std::path::Path) -> Vec<String> {
    use tyc_syntax::preprocess;
    let file = if path.is_dir() {
        path.join("__init__.ty")
    } else {
        path.to_path_buf()
    };
    let Ok(source) = std::fs::read_to_string(&file) else {
        return Vec::new();
    };
    preprocess::preprocess(&source).pub_names
}

/// The `args` tuple for an exception reconstructed from a `VmException` that
/// carries only a message string: a 1-tuple of the message, or empty.
fn exc_fallback_args(message: &str) -> Vec<Value> {
    if message.is_empty() {
        Vec::new()
    } else {
        vec![Value::Str(Rc::new(message.to_owned()))]
    }
}

fn builtin_exc_is_a(kind: &str, target: &str) -> bool {
    if target == "BaseException" {
        return true;
    }
    if target == "Exception" {
        // Everything except the BaseException-only siblings.
        return !matches!(kind, "KeyboardInterrupt" | "SystemExit" | "GeneratorExit");
    }
    // Direct parent in the standard hierarchy (subset covering the common
    // intermediate bases programs actually catch).
    fn parent(name: &str) -> Option<&'static str> {
        Some(match name {
            "ZeroDivisionError" | "OverflowError" | "FloatingPointError" => "ArithmeticError",
            "IndexError" | "KeyError" => "LookupError",
            "ModuleNotFoundError" => "ImportError",
            "RecursionError" | "NotImplementedError" => "RuntimeError",
            "UnboundLocalError" => "NameError",
            "UnicodeError" => "ValueError",
            "UnicodeDecodeError" | "UnicodeEncodeError" | "UnicodeTranslateError" => "UnicodeError",
            "FileNotFoundError" | "FileExistsError" | "PermissionError" | "IsADirectoryError"
            | "NotADirectoryError" | "InterruptedError" | "TimeoutError" | "BlockingIOError"
            | "ChildProcessError" | "ProcessLookupError" | "ConnectionError" => "OSError",
            "BrokenPipeError"
            | "ConnectionResetError"
            | "ConnectionRefusedError"
            | "ConnectionAbortedError" => "ConnectionError",
            _ => return None,
        })
    }
    let mut cur = kind;
    while let Some(p) = parent(cur) {
        if p == target {
            return true;
        }
        cur = p;
    }
    false
}

/// Collect the names of walrus (`:=`) assignment targets appearing anywhere
/// in `e`. Used so a walrus inside a comprehension leaks its target into the
/// enclosing scope (Python semantics).
fn collect_walrus_names(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Named(n) => {
            if let Expr::Name(name) = n.target.as_ref() {
                out.push(name.id.as_str().to_owned());
            }
            collect_walrus_names(&n.value, out);
        }
        Expr::BoolOp(b) => {
            for v in &b.values {
                collect_walrus_names(v, out);
            }
        }
        Expr::BinOp(b) => {
            collect_walrus_names(&b.left, out);
            collect_walrus_names(&b.right, out);
        }
        Expr::UnaryOp(u) => collect_walrus_names(&u.operand, out),
        Expr::Compare(c) => {
            collect_walrus_names(&c.left, out);
            for c2 in c.comparators.iter() {
                collect_walrus_names(c2, out);
            }
        }
        Expr::Call(c) => {
            collect_walrus_names(&c.func, out);
            for a in c.arguments.args.iter() {
                collect_walrus_names(a, out);
            }
            for kw in c.arguments.keywords.iter() {
                collect_walrus_names(&kw.value, out);
            }
        }
        Expr::If(t) => {
            collect_walrus_names(&t.test, out);
            collect_walrus_names(&t.body, out);
            collect_walrus_names(&t.orelse, out);
        }
        Expr::Subscript(s) => {
            collect_walrus_names(&s.value, out);
            collect_walrus_names(&s.slice, out);
        }
        Expr::Attribute(a) => collect_walrus_names(&a.value, out),
        Expr::Starred(s) => collect_walrus_names(&s.value, out),
        Expr::Tuple(t) => {
            for x in &t.elts {
                collect_walrus_names(x, out);
            }
        }
        Expr::List(l) => {
            for x in &l.elts {
                collect_walrus_names(x, out);
            }
        }
        Expr::Set(s) => {
            for x in &s.elts {
                collect_walrus_names(x, out);
            }
        }
        _ => {}
    }
}

/// Walrus target names from a comprehension's element expression(s) plus the
/// `if` filters on every generator (the bound-iterable expressions are
/// evaluated in the enclosing scope already, so their walruses leak anyway).
fn comprehension_walrus_names(parts: &[&Expr], generators: &[ast::Comprehension]) -> Vec<String> {
    let mut out = Vec::new();
    for p in parts {
        collect_walrus_names(p, &mut out);
    }
    for g in generators {
        for cond in &g.ifs {
            collect_walrus_names(cond, &mut out);
        }
    }
    out
}

fn class_is_subclass(c: &Rc<Class>, target: &Rc<Class>) -> bool {
    if Rc::ptr_eq(c, target) {
        return true;
    }
    c.bases.iter().any(|b| class_is_subclass(b, target))
}

/// Re-format Rust's `{:e}` exponent notation to match CPython's output:
/// - Exponent always has an explicit sign (`+` or `-`)
/// - Exponent is zero-padded to at least 2 digits
/// - If `upper` is true, convert `e` → `E`
///
/// Example: `"3.141590e0"` → `"3.141590e+00"`.
/// (FINDINGS: Rust's `{:.6e}` gives `e0`, CPython gives `e+00`.)
fn normalise_exp_notation(s: String, upper: bool) -> String {
    // Find the `e` marker (Rust always uses lowercase in `{:e}`).
    let e_pos = match s.find('e') {
        Some(p) => p,
        None => return s,
    };
    let mantissa = &s[..e_pos];
    let exp_str = &s[e_pos + 1..]; // may be `0`, `-4`, `+4`, `100`, ...
    let (sign, digits) = if let Some(rest) = exp_str.strip_prefix('-') {
        ("-", rest)
    } else if let Some(rest) = exp_str.strip_prefix('+') {
        ("+", rest)
    } else {
        ("+", exp_str)
    };
    // Zero-pad to at least 2 digits.
    let padded = if digits.len() < 2 {
        format!("{:0>2}", digits)
    } else {
        digits.to_owned()
    };
    let e_char = if upper { 'E' } else { 'e' };
    format!("{mantissa}{e_char}{sign}{padded}")
}

/// CPython-compatible `g`/`G` float formatting.
///
/// Rules (PEP 3101 / C printf `%g`):
/// 1. Use scientific notation when the exponent is < −4 or ≥ precision.
/// 2. Trailing zeros after the decimal point are removed.
/// 3. The decimal point is removed if there are no remaining digits after it.
/// 4. Exponent sign and 2-digit pad follow the same rules as `e`.
fn format_g(abs: f64, sig: usize, upper: bool) -> String {
    // Format in scientific notation to determine the exponent.
    let sci = format!("{:.*e}", sig.saturating_sub(1), abs);
    // Parse out the exponent.
    let exp: i32 = if let Some(pos) = sci.find('e') {
        sci[pos + 1..].parse().unwrap_or(0)
    } else {
        0
    };
    let result = if exp < -4 || exp >= sig as i32 {
        // Scientific notation path.
        let raw = normalise_exp_notation(sci, upper);
        // Strip trailing zeros from mantissa part before `e`.
        let e_pos = raw.find(if upper { 'E' } else { 'e' }).unwrap_or(raw.len());
        let mantissa = raw[..e_pos].trim_end_matches('0').trim_end_matches('.');
        format!("{}{}", mantissa, &raw[e_pos..])
    } else {
        // Fixed notation: format with enough decimal places, then strip zeros.
        let decimal_places = (sig as i32 - 1 - exp).max(0) as usize;
        let fixed = format!("{:.*}", decimal_places, abs);
        // Strip trailing zeros after decimal point.
        if fixed.contains('.') {
            let stripped = fixed.trim_end_matches('0').trim_end_matches('.');
            stripped.to_owned()
        } else {
            fixed
        }
    };
    result
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

    // Percentage type `%` (M3): multiply by 100, format with the given
    // precision (default 6), append `%`. Works for any numeric value
    // (e.g. `f"{1:.2%}"` → `100.00%`).
    if typ == Some('%') && is_numeric {
        let x = value.to_float()?;
        let scaled = x * 100.0;
        let p = precision.unwrap_or(6);
        let (abs, neg) = (scaled.abs(), scaled.is_sign_negative());
        let body = format!("{:.*}%", p, abs);
        if neg {
            explicit_sign.push('-');
        } else if let Some('+') = sign {
            explicit_sign.push('+');
        } else if let Some(' ') = sign {
            explicit_sign.push(' ');
        }
        if let Some(w) = width {
            let total_len = explicit_sign.chars().count() + body.chars().count();
            if total_len < w {
                let pad = w - total_len;
                let eff_align = align.unwrap_or('>');
                let p_str: String = std::iter::repeat_n(fill, pad).collect();
                return Ok(match eff_align {
                    '<' => format!("{explicit_sign}{body}{p_str}"),
                    '^' => {
                        let lo = pad / 2;
                        let hi = pad - lo;
                        let lo_s: String = std::iter::repeat_n(fill, lo).collect();
                        let hi_s: String = std::iter::repeat_n(fill, hi).collect();
                        format!("{lo_s}{explicit_sign}{body}{hi_s}")
                    }
                    '=' => format!("{explicit_sign}{p_str}{body}"),
                    _ => format!("{p_str}{explicit_sign}{body}"),
                });
            }
        }
        return Ok(format!("{explicit_sign}{body}"));
    }

    // Float-presentation types (`e`/`E`/`f`/`F`/`g`/`G`) coerce an int or
    // bool operand to float, matching CPython (`f"{42:.2f}"` → "42.00").
    let coerced;
    let value: &Value = if matches!(typ, Some('e' | 'E' | 'f' | 'F' | 'g' | 'G'))
        && matches!(value, Value::Int(_) | Value::Bool(_))
    {
        coerced = Value::Float(value.to_float()?);
        &coerced
    } else {
        value
    };

    match value {
        Value::Float(x) => {
            let p = precision.unwrap_or(6);
            let (abs, neg) = (x.abs(), *x < 0.0 || x.is_sign_negative());
            // CPython formats NaN and inf with lowercase letters regardless of
            // type specifier (even `E` gives `nan`/`inf`). Rust emits `NaN`
            // which differs. Handle them specially before the general branch.
            let raw: String = if x.is_nan() {
                "nan".to_owned()
            } else if x.is_infinite() {
                "inf".to_owned()
            } else {
                match typ {
                    Some('e') => {
                        // Rust's {:e} produces e.g. `3.141590e0`; CPython requires
                        // at least 2 exponent digits with an explicit sign: `3.141590e+00`.
                        normalise_exp_notation(format!("{:.*e}", p, abs), false)
                    }
                    Some('E') => normalise_exp_notation(format!("{:.*e}", p, abs), true),
                    Some('g') | Some('G') => {
                        // Python's `g` uses precision as significant digits.
                        // After computing the exponential form we apply CPython's
                        // `g` rules: strip trailing zeros in the mantissa, then
                        // decide whether to render as fixed or scientific notation.
                        let sig = if p == 0 { 1 } else { p };
                        let upper = matches!(typ, Some('G'));
                        format_g(abs, sig, upper)
                    }
                    _ => format!("{:.*}", p, abs),
                }
            };
            buf = if comma || underscore {
                let sep = if comma { ',' } else { '_' };
                insert_float_thousands(&raw, sep)
            } else {
                raw
            };
            if neg && !x.is_nan() {
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
                Some('c') => {
                    // `{n:c}` → the Unicode character at codepoint n.
                    let cp = i_val
                        .to_u32()
                        .and_then(char::from_u32)
                        .ok_or_else(|| value_error("%c arg not in range(0x110000)"))?;
                    buf = cp.to_string();
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

/// Public wrapper so `builtins.rs` can call the format-spec engine for
/// `str.format()` without needing to re-implement or duplicate the logic.
pub fn format_with_spec_pub(
    value: &crate::value::Value,
    default: &str,
    spec: &str,
) -> Result<String, crate::error::Unwind> {
    format_with_spec(value, default, spec)
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
    fn generator_eager_collection_runs() {
        // Generators are materialised eagerly: iterating one yields its values,
        // including `yield from`.
        let src = r#"
def gen():
    for i in range(3):
        yield i * i

def flat():
    yield from [1, 2]
    yield 3

squares = list(gen())
flattened = list(flat())
"#;
        let (interp, res) = parse_and_run(src);
        res.expect("generator should run");
        assert_eq!(interp.root.get("squares").unwrap().py_str(), "[0, 1, 4]");
        assert_eq!(interp.root.get("flattened").unwrap().py_str(), "[1, 2, 3]");
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

    /// Mechanism #1 — instance operator-overload dispatch: `a + b` on two
    /// `Vec2` instances must invoke `__add__` and walk the MRO / bind self.
    #[test]
    fn instance_add_dunder_dispatch() {
        let src = r#"
class Vec2:
    def __init__(self, x, y):
        self.x = x
        self.y = y
    def __add__(self, other):
        return Vec2(self.x + other.x, self.y + other.y)

a = Vec2(1, 2)
b = Vec2(3, 4)
c = a + b
rx = c.x
ry = c.y
"#;
        let (interp, res) = parse_and_run(src);
        res.unwrap();
        assert_eq!(interp.root.get("rx").unwrap().py_str(), "4");
        assert_eq!(interp.root.get("ry").unwrap().py_str(), "6");
    }

    /// Mechanism #1 (reflected) — `2 * v` with no `__mul__` on int falls back
    /// to the right operand's `__rmul__`.
    #[test]
    fn instance_reflected_dunder_dispatch() {
        let src = r#"
class Scalar:
    def __init__(self, v):
        self.v = v
    def __rmul__(self, other):
        return Scalar(self.v * other)

s = 3 * Scalar(4)
r = s.v
"#;
        let (interp, res) = parse_and_run(src);
        res.unwrap();
        assert_eq!(interp.root.get("r").unwrap().py_str(), "12");
    }

    /// Mechanism #2 — `__missing__` is invoked on a missing key and the
    /// returned default is stored under the key.
    #[test]
    fn subscript_missing_hook_stores_default() {
        let src = r#"
class DefaultMap:
    def __init__(self):
        self.store = {}
    def __getitem__(self, key):
        if key in self.store:
            return self.store[key]
        raise KeyError(key)
    def __setitem__(self, key, value):
        self.store[key] = value
    def __missing__(self, key):
        return 99

d = DefaultMap()
first = d["absent"]
again = d["absent"]
"#;
        let (interp, res) = parse_and_run(src);
        res.unwrap();
        assert_eq!(interp.root.get("first").unwrap().py_str(), "99");
        // Second read hits the stored value, not __missing__ again.
        assert_eq!(interp.root.get("again").unwrap().py_str(), "99");
    }

    /// Mechanism #3 — complex arithmetic and CPython-exact repr.
    #[test]
    fn complex_arithmetic_and_repr() {
        // repr forms
        assert_eq!(Value::Complex(3.0, 4.0).py_str(), "(3+4j)");
        assert_eq!(Value::Complex(0.0, 4.0).py_str(), "4j");
        assert_eq!(Value::Complex(1.0, -2.0).py_str(), "(1-2j)");
        assert_eq!(Value::Complex(3.0, 0.0).py_str(), "(3+0j)");
        assert_eq!(Value::Complex(1.5, 2.5).py_str(), "(1.5+2.5j)");
        assert_eq!(Value::Complex(0.0, -1.0).py_str(), "-1j");

        let src = r#"
a = 1 + 2j
b = (1+2j) * (3+4j)
c = 4j
d = 10j / 2j
e = (1+2j) - (3+1j)
"#;
        let (interp, res) = parse_and_run(src);
        res.unwrap();
        assert_eq!(interp.root.get("a").unwrap().py_str(), "(1+2j)");
        assert_eq!(interp.root.get("b").unwrap().py_str(), "(-5+10j)");
        assert_eq!(interp.root.get("c").unwrap().py_str(), "4j");
        assert_eq!(interp.root.get("d").unwrap().py_str(), "(5+0j)");
        assert_eq!(interp.root.get("e").unwrap().py_str(), "(-2+1j)");
    }

    /// Mechanism #3 — complex equality across int/float when imag == 0.
    #[test]
    fn complex_equality() {
        assert!(Value::Complex(3.0, 0.0).py_eq(&Value::Int(BigInt::from(3))));
        assert!(Value::Complex(3.0, 0.0).py_eq(&Value::Float(3.0)));
        assert!(!Value::Complex(3.0, 1.0).py_eq(&Value::Int(BigInt::from(3))));
        assert!(Value::Complex(1.0, 2.0).py_eq(&Value::Complex(1.0, 2.0)));
    }

    /// Mechanism #4 — dict-view repr, len, iteration, and membership.
    #[test]
    fn dict_view_repr_iter_len_contains() {
        use crate::value::DictViewKind;
        let keys = Value::DictView {
            kind: DictViewKind::Keys,
            items: vec![
                Value::Str(Rc::new("a".into())),
                Value::Str(Rc::new("b".into())),
            ],
        };
        assert_eq!(keys.py_str(), "dict_keys(['a', 'b'])");
        assert_eq!(keys.type_name(), "dict_keys");

        let values = Value::DictView {
            kind: DictViewKind::Values,
            items: vec![Value::Int(BigInt::from(1)), Value::Int(BigInt::from(2))],
        };
        assert_eq!(values.py_str(), "dict_values([1, 2])");

        let items = Value::DictView {
            kind: DictViewKind::Items,
            items: vec![
                Value::Tuple(Rc::new(vec![
                    Value::Str(Rc::new("a".into())),
                    Value::Int(BigInt::from(1)),
                ])),
                Value::Tuple(Rc::new(vec![
                    Value::Str(Rc::new("b".into())),
                    Value::Int(BigInt::from(2)),
                ])),
            ],
        };
        assert_eq!(items.py_str(), "dict_items([('a', 1), ('b', 2)])");

        // Iteration via make_iter / iter_next.
        let mut interp = Interpreter::new();
        let it = interp.make_iter(keys.clone()).unwrap();
        let mut out = Vec::new();
        while let Some(v) = interp.iter_next(&it).unwrap() {
            out.push(v.py_str());
        }
        assert_eq!(out, vec!["a", "b"]);

        // Membership.
        assert!(interp
            .contains(&keys, &Value::Str(Rc::new("a".into())))
            .unwrap());
        assert!(!interp
            .contains(&keys, &Value::Str(Rc::new("z".into())))
            .unwrap());
    }

    // ── builtins agent: stdlib shims ───────────────────────────────────────

    /// M5 — `defaultdict(list)` auto-creates a default for a missing key via
    /// the `__missing__` hook, and `dict(dd)` materialises the mapping.
    #[test]
    fn defaultdict_factory_auto_default() {
        let src = r#"
from collections import defaultdict
groups = defaultdict(list)
groups["even"].append(2)
groups["even"].append(4)
out = dict(groups)
n = len(groups)
present = "even" in groups
keys = list(groups)
ic = defaultdict(int)
ic["x"] += 5
iv = ic["x"]
"#;
        let (interp, res) = parse_and_run(src);
        res.unwrap();
        assert_eq!(interp.root.get("out").unwrap().py_str(), "{'even': [2, 4]}");
        assert_eq!(interp.root.get("n").unwrap().py_str(), "1");
        assert_eq!(interp.root.get("present").unwrap().py_str(), "True");
        assert_eq!(interp.root.get("keys").unwrap().py_str(), "['even']");
        assert_eq!(interp.root.get("iv").unwrap().py_str(), "5");
    }

    /// M15 — `datetime` arithmetic: `datetime + timedelta` and `date - date`.
    #[test]
    fn datetime_arithmetic() {
        let src = r#"
from datetime import datetime, timedelta, date
d = date(2026, 6, 3)
iso = d.isoformat()
dt = datetime(2026, 1, 1, 12, 30)
later = dt + timedelta(days=10)
day = later.day
delta = (date(2026, 6, 3) - date(2026, 1, 1)).days
hh = dt.hour
mm = dt.minute
"#;
        let (interp, res) = parse_and_run(src);
        res.unwrap();
        assert_eq!(interp.root.get("iso").unwrap().py_str(), "2026-06-03");
        assert_eq!(interp.root.get("day").unwrap().py_str(), "11");
        assert_eq!(interp.root.get("delta").unwrap().py_str(), "153");
        assert_eq!(interp.root.get("hh").unwrap().py_str(), "12");
        assert_eq!(interp.root.get("mm").unwrap().py_str(), "30");
    }

    /// M16 — `pathlib.Path` `/` join + `.suffixes`.
    #[test]
    fn pathlib_join_and_suffixes() {
        let src = r#"
from pathlib import Path
joined = (Path("/a") / "b" / "c.txt").name
sfx = Path("file.tar.gz").suffixes
"#;
        let (interp, res) = parse_and_run(src);
        res.unwrap();
        assert_eq!(interp.root.get("joined").unwrap().py_str(), "c.txt");
        assert_eq!(interp.root.get("sfx").unwrap().py_str(), "['.tar', '.gz']");
    }

    /// M17 — `complex()` constructor, `abs(complex)`, and `.real` / `.imag`.
    #[test]
    fn complex_constructor_and_attrs() {
        let src = r#"
c = complex(3, 4)
mag = abs(c)
re = c.real
im = c.imag
z = complex()
one = complex(7)
"#;
        let (interp, res) = parse_and_run(src);
        res.unwrap();
        assert_eq!(interp.root.get("mag").unwrap().py_str(), "5.0");
        assert_eq!(interp.root.get("re").unwrap().py_str(), "3.0");
        assert_eq!(interp.root.get("im").unwrap().py_str(), "4.0");
        assert_eq!(interp.root.get("z").unwrap().py_str(), "0j");
        assert_eq!(interp.root.get("one").unwrap().py_str(), "(7+0j)");
    }

    /// L3 — `dict.keys()/.values()/.items()` return dict-views (not lists).
    #[test]
    fn dict_methods_return_dict_views() {
        let src = r#"
d = {"a": 1, "b": 2}
k = d.keys()
v = d.values()
it = d.items()
"#;
        let (interp, res) = parse_and_run(src);
        res.unwrap();
        assert!(matches!(
            interp.root.get("k").unwrap(),
            Value::DictView {
                kind: crate::value::DictViewKind::Keys,
                ..
            }
        ));
        assert_eq!(
            interp.root.get("k").unwrap().py_str(),
            "dict_keys(['a', 'b'])"
        );
        assert_eq!(
            interp.root.get("v").unwrap().py_str(),
            "dict_values([1, 2])"
        );
        assert_eq!(
            interp.root.get("it").unwrap().py_str(),
            "dict_items([('a', 1), ('b', 2)])"
        );
    }
}
