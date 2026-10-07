//! Callable contracts with source provenance; no changes to flow state.
use super::*;
use ruff_python_ast::visitor::{self, Visitor};

pub(super) fn parameter_pack(params: &[Type]) -> Option<&str> {
    match params {
        [Type::TypeVar(name)] => name.strip_prefix("**"),
        _ => None,
    }
}

pub(super) fn constructor(c: &Checker, name: &str) -> Option<Type> {
    let name = class_name(c, name)?;
    let name = name.as_str();
    let shape = effective_class_shape(name, &c.class_shapes)?;
    let info = shape
        .methods
        .get("__init__")
        .map(|sig| sig.arity_info.clone())
        .unwrap_or_else(|| class_constructor_arity_for(&shape, Some(name)));
    if !info.kwonly_required.is_empty() {
        return None;
    }
    Some(Type::Function {
        params: info.param_types,
        ret: Box::new(Type::Class(name.into())),
        variadic: info.max_positional.is_none(),
        min_params: Some(info.min_positional),
    })
}

/// Whether the bare name `name` reads the module-level function or import
/// that the name-keyed tables (`function_arity_info`, `function_signatures`,
/// `function_type_bounds`, …) describe: the binding in scope is that
/// module-scope `def` / import, or nothing binds the name. A parameter, a
/// local, a loop target or a nested `def` of the same name is some other
/// callable.
pub(super) fn binds_module_function(c: &Checker, name: &str) -> bool {
    if binds_locally(c, name) {
        return false;
    }
    let Some(binding) = c.env.lookup(name) else {
        return true;
    };
    c.resolved.scopes.first().is_some_and(|s| {
        s.bindings.iter().any(|b| {
            b.name == name
                && b.span.0 == binding.span.0
                && matches!(b.kind, BindingKind::Function | BindingKind::Import)
        })
    })
}

/// Whether the body of the function being checked binds `name` itself, which
/// makes it a local there for the whole body — a walrus does so even where
/// the env still narrows the module binding in place.
pub(super) fn binds_locally(c: &Checker, name: &str) -> bool {
    c.function_scope != 0
        && c.resolved
            .scopes
            .get(c.function_scope)
            .is_some_and(|s| s.lookup_local(name).is_some())
}

/// Whether an assignment to `name` in the function body being checked binds
/// a new local although the env shows an enclosing binding of that name (a
/// module-level `def`, import or global, or an outer function's local).
/// Python makes every name a body assigns local to the whole body unless it
/// is declared `global` / `nonlocal`, which the resolver does not record as
/// a local binding.
pub(super) fn assignment_binds_local(c: &Checker, name: &str) -> bool {
    match (c.env.scope_of(name), c.env.function_frame()) {
        (Some(found), Some(frame)) => found < frame && binds_locally(c, name),
        _ => false,
    }
}

/// Where `name`'s binding in scope was declared: the env's, or — when the
/// env still shows the module binding although the function body binds
/// `name` itself (see [`binds_locally`]) — the body's own.
fn binding_start(c: &Checker, name: &str) -> Option<usize> {
    if c.env.scope_of(name) == Some(0) && binds_locally(c, name) {
        return c
            .resolved
            .scopes
            .get(c.function_scope)?
            .lookup_local(name)
            .map(|b| b.span.0);
    }
    c.env.lookup(name).map(|b| b.span.0)
}

pub(super) fn origin(c: &Checker, expr: &Expr) -> Option<ArityInfo> {
    fn resolve(c: &Checker, expr: &Expr, seen: &mut HashSet<usize>) -> Option<ArityInfo> {
        // `let g = u.greet` / `let f = helpers.parse`: the alias calls what
        // the attribute resolved to where it was defined — `u` may name
        // something else at the call. An alias not checked yet records
        // nothing, and the call keeps its structural check.
        if let Expr::Attribute(_) = expr {
            return c
                .attr_callee_arity
                .get(&expr_span(expr))
                .filter(|(_, known)| *known)
                .map(|(info, _)| info.clone());
        }
        if let Expr::Lambda(lam) = expr {
            return lam
                .parameters
                .as_deref()
                .map(|p| arity_info_from_parameters(p, &c.classes, &[]));
        }
        if let Expr::Call(call) = expr {
            if let Type::Function { params, ret, .. } = infer_expr_readonly(c, &call.func) {
                if let Type::Function { params: output, .. } = ret.as_ref() {
                    if let Some(pack) = parameter_pack(output) {
                        for (i, param) in params.iter().enumerate() {
                            if matches!(param, Type::Function{params,..} if parameter_pack(params)==Some(pack))
                            {
                                let arg = call.arguments.args.get(i).or_else(|| {
                                    let info = origin(c, &call.func)?;
                                    let name = info.param_names.get(i)?;
                                    call.arguments
                                        .keywords
                                        .iter()
                                        .find(|kw| {
                                            kw.arg.as_ref().is_some_and(|n| n.as_str() == name)
                                        })
                                        .map(|kw| &kw.value)
                                });
                                if let Some(arg) = arg {
                                    return resolve(c, arg, seen);
                                }
                            }
                        }
                    }
                }
            }
        }
        let Expr::Name(n) = expr else { return None };
        let start = binding_start(c, n.id.as_str())?;
        if !seen.insert(start) {
            return None;
        }
        struct Find<'a> {
            start: usize,
            value: Option<&'a Expr>,
            function: Option<&'a ruff_python_ast::StmtFunctionDef>,
        }
        impl<'a> Visitor<'a> for Find<'a> {
            fn visit_stmt(&mut self, stmt: &'a Stmt) {
                match stmt {
                    Stmt::FunctionDef(f) if f.name.range.start().to_usize() == self.start => {
                        self.function = Some(f)
                    }
                    Stmt::AnnAssign(a) if a.target.range().start().to_usize() == self.start => {
                        self.value = a.value.as_deref()
                    }
                    Stmt::Assign(a)
                        if a.targets
                            .iter()
                            .any(|t| t.range().start().to_usize() == self.start) =>
                    {
                        self.value = Some(&a.value)
                    }
                    _ => visitor::walk_stmt(self, stmt),
                }
            }
        }
        let mut find = Find {
            start,
            value: None,
            function: None,
        };
        if let Some(module) = c.module {
            for stmt in &module.body {
                find.visit_stmt(stmt);
            }
        }
        if let Some(f) = find.function {
            return Some(arity_info_from_parameters(
                &f.parameters,
                &c.classes,
                &type_param_names_from(f.type_params.as_deref()),
            ));
        }
        if let Some(value) = find.value {
            return resolve(c, value, seen);
        }
        // An imported function. A parameter, loop target or other untraced
        // local of the same name holds some other callable: its own
        // (structural) type is all that is known about it.
        if binds_module_function(c, n.id.as_str()) {
            return c.function_arity_info.get(n.id.as_str()).cloned();
        }
        None
    }
    // The outermost expression is the callee itself: an attribute callee's
    // arity comes from `infer_expr`, which resolved its receiver for real.
    if matches!(expr, Expr::Attribute(_)) {
        return None;
    }
    resolve(c, expr, &mut HashSet::new())
}

pub(super) fn union_call(
    c: &mut Checker,
    call: &ruff_python_ast::ExprCall,
    members: &[Type],
) -> Option<Type> {
    if !members.iter().all(|m| matches!(m, Type::Function { .. })) {
        return None;
    }
    let span = (call.range.start().to_usize(), call.range.end().to_usize());
    let actuals: Vec<Type> = call
        .arguments
        .args
        .iter()
        .map(|a| infer_expr(c, a))
        .collect();
    let keywords = &call.arguments.keywords;
    for kw in keywords {
        let _ = infer_expr(c, &kw.value);
    }
    // A function type records no parameter names, so a keyword can fill any
    // parameter of a variant, and a `*`/`**` unpacking supplies an unknown
    // number of arguments: the only count left to check is too many
    // positionals. Positionals after a `*` unpacking have no fixed slot.
    let star = call
        .arguments
        .args
        .iter()
        .position(|a| matches!(a, Expr::Starred(_)));
    let positional = &actuals[..star.unwrap_or(actuals.len())];
    let open = !keywords.is_empty() || star.is_some();
    let mut returns = Vec::new();
    for member in members {
        let Type::Function {
            params,
            ret,
            variadic,
            min_params,
        } = member
        else {
            unreachable!()
        };
        if (!open && actuals.len() < min_params.unwrap_or(params.len()))
            || (!variadic && positional.len() > params.len())
        {
            c.wrong_args("<callable variant>", params.len(), actuals.len(), span);
        }
        for (param, actual) in params.iter().zip(positional) {
            if !c.is_assignable(param, actual) {
                c.mismatch(param, actual, span);
            }
        }
        returns.push(bind_typevars_and_substitute(params, positional, ret));
    }
    Some(Type::union_of(returns))
}

/// Decorators may replace a synchronous function with an awaitable factory.
pub(super) fn decorated(c: &Checker, name: &str) -> bool {
    struct Scan<'s> {
        name: &'s str,
        found: bool,
    }
    impl<'a> Visitor<'a> for Scan<'_> {
        fn visit_stmt(&mut self, stmt: &'a Stmt) {
            if let Stmt::FunctionDef(f) = stmt {
                if f.name.as_str() == self.name && !f.decorator_list.is_empty() {
                    self.found = true;
                }
            }
            visitor::walk_stmt(self, stmt);
        }
    }
    let Some(module) = c.module else { return false };
    let mut scan = Scan { name, found: false };
    for stmt in &module.body {
        scan.visit_stmt(stmt);
    }
    scan.found
}

/// Whether a class named `name` carries a decorator, which may replace it.
pub(super) fn class_decorated(c: &Checker, name: &str) -> bool {
    struct Scan<'s> {
        name: &'s str,
        found: bool,
    }
    impl<'a> Visitor<'a> for Scan<'_> {
        fn visit_stmt(&mut self, stmt: &'a Stmt) {
            if let Stmt::ClassDef(cd) = stmt {
                if cd.name.as_str() == self.name && !cd.decorator_list.is_empty() {
                    self.found = true;
                }
            }
            visitor::walk_stmt(self, stmt);
        }
    }
    let Some(module) = c.module else { return false };
    let mut scan = Scan { name, found: false };
    for stmt in &module.body {
        scan.visit_stmt(stmt);
    }
    scan.found
}

/// Every `def` of `name` in the class body (and `impl` blocks) that
/// `cls.name` resolves to among this module's top-level classes, in `cls`'s
/// C3 method resolution order: a later `def` in the same body replaces an
/// earlier one at runtime, so a caller judging decorators sees them all.
/// `None` when the order cannot be computed, when it reaches a class whose
/// body is not here before finding the method, or when it never finds it.
pub(super) fn method_defs<'a>(
    c: &Checker<'a>,
    cls: &str,
    name: &str,
) -> Option<Vec<&'a ruff_python_ast::StmtFunctionDef>> {
    let module = c.module?;
    for current in tyc_syntax::mro::c3_linearise(cls, &c.class_parents)? {
        let pseudo = format!("__typhon_impl_{current}");
        let mut declared = false;
        let mut defs = Vec::new();
        for stmt in &module.body {
            let Stmt::ClassDef(cd) = stmt else { continue };
            if cd.name.as_str() != current && cd.name.as_str() != pseudo {
                continue;
            }
            declared = true;
            defs_named(&cd.body, name, &mut defs);
        }
        if !declared {
            return None;
        }
        if !defs.is_empty() {
            return Some(defs);
        }
    }
    None
}

/// The defs named `name` in a class body, through its compound statements
/// (`if FAST: def run…`, `try: … except: def run…`) but not into a nested
/// `def` or `class`.
fn defs_named<'a>(
    body: &'a [Stmt],
    name: &str,
    out: &mut Vec<&'a ruff_python_ast::StmtFunctionDef>,
) {
    for stmt in body {
        match stmt {
            Stmt::FunctionDef(f) if f.name.as_str() == name => out.push(f),
            Stmt::If(s) => {
                defs_named(&s.body, name, out);
                for clause in &s.elif_else_clauses {
                    defs_named(&clause.body, name, out);
                }
            }
            Stmt::Try(s) => {
                defs_named(&s.body, name, out);
                for handler in &s.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = handler;
                    defs_named(&h.body, name, out);
                }
                defs_named(&s.orelse, name, out);
                defs_named(&s.finalbody, name, out);
            }
            Stmt::With(s) => defs_named(&s.body, name, out),
            Stmt::For(s) => {
                defs_named(&s.body, name, out);
                defs_named(&s.orelse, name, out);
            }
            Stmt::While(s) => {
                defs_named(&s.body, name, out);
                defs_named(&s.orelse, name, out);
            }
            Stmt::Match(s) => {
                for case in &s.cases {
                    defs_named(&case.body, name, out);
                }
            }
            _ => {}
        }
    }
}

pub(super) fn alias_value<'a>(c: &Checker<'a>, start: usize) -> Option<&'a Expr> {
    struct Find<'a> {
        start: usize,
        value: Option<&'a Expr>,
    }
    impl<'a> Visitor<'a> for Find<'a> {
        fn visit_stmt(&mut self, stmt: &'a Stmt) {
            match stmt {
                Stmt::AnnAssign(a) if a.target.range().start().to_usize() == self.start => {
                    self.value = a.value.as_deref()
                }
                Stmt::Assign(a)
                    if a.targets
                        .iter()
                        .any(|t| t.range().start().to_usize() == self.start) =>
                {
                    self.value = Some(&a.value)
                }
                _ => visitor::walk_stmt(self, stmt),
            }
        }
    }
    let mut find = Find { start, value: None };
    for stmt in &c.module?.body {
        find.visit_stmt(stmt);
    }
    find.value
}
pub(super) fn class_name(c: &Checker, name: &str) -> Option<String> {
    fn resolve(c: &Checker, name: &str, seen: &mut HashSet<usize>) -> Option<String> {
        let binding = c.env.lookup(name)?;
        if !seen.insert(binding.span.0) {
            return None;
        }
        let resolved = c
            .resolved
            .scopes
            .iter()
            .flat_map(|s| &s.bindings)
            .find(|b| b.name == name && b.span.0 == binding.span.0)?;
        if matches!(resolved.kind, BindingKind::Class | BindingKind::Import)
            && c.classes.iter().any(|n| n == name)
        {
            return Some(name.to_owned());
        }
        if let Expr::Name(n) = alias_value(c, binding.span.0)? {
            return resolve(c, n.id.as_str(), seen);
        }
        None
    }
    resolve(c, name, &mut HashSet::new())
}

pub(super) fn known_sync(c: &Checker, expr: &Expr) -> bool {
    fn resolve(c: &Checker, expr: &Expr, seen: &mut HashSet<usize>) -> bool {
        if matches!(expr, Expr::Lambda(_)) {
            return true;
        }
        let Expr::Name(n) = expr else { return false };
        let Some(binding) = c.env.lookup(n.id.as_str()) else {
            return false;
        };
        if !seen.insert(binding.span.0) {
            return false;
        }
        struct Find {
            start: usize,
            sync: Option<bool>,
        }
        impl<'a> Visitor<'a> for Find {
            fn visit_stmt(&mut self, stmt: &'a Stmt) {
                if let Stmt::FunctionDef(f) = stmt {
                    if f.name.range.start().to_usize() == self.start {
                        self.sync = Some(!f.is_async && f.decorator_list.is_empty());
                        return;
                    }
                }
                visitor::walk_stmt(self, stmt);
            }
        }
        let mut find = Find {
            start: binding.span.0,
            sync: None,
        };
        if let Some(module) = c.module {
            for stmt in &module.body {
                find.visit_stmt(stmt);
            }
        }
        if let Some(sync) = find.sync {
            return sync;
        }
        if let Some(value) = alias_value(c, binding.span.0) {
            return resolve(c, value, seen);
        }
        // `from helpers import work`, where `work` is a plain `def` in a
        // project `.ty` module (W3: imported known-sync callees).
        let name = n.id.as_str();
        if c.imported_sync_functions.contains(name)
            && c.resolved.scopes.first().is_some_and(|s| {
                s.bindings.iter().any(|b| {
                    b.name == name && b.span.0 == binding.span.0 && b.kind == BindingKind::Import
                })
            })
        {
            return true;
        }
        let parameter = c
            .resolved
            .scopes
            .iter()
            .flat_map(|s| &s.bindings)
            .any(|b| b.span.0 == binding.span.0 && b.kind == BindingKind::Parameter);
        parameter
            && matches!(&binding.narrowed,Type::Function{ret,..} if definitely_not_awaitable(c,ret))
    }
    resolve(c, expr, &mut HashSet::new())
}
