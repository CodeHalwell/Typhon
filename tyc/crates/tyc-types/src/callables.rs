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

pub(super) fn origin(c: &Checker, expr: &Expr) -> Option<ArityInfo> {
    fn resolve(c: &Checker, expr: &Expr, seen: &mut HashSet<usize>) -> Option<ArityInfo> {
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
        let binding = c.env.lookup(n.id.as_str())?;
        if !seen.insert(binding.span.0) {
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
            start: binding.span.0,
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
        c.function_arity_info.get(n.id.as_str()).cloned()
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

fn alias_value<'a>(c: &Checker<'a>, start: usize) -> Option<&'a Expr> {
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
