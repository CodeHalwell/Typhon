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
    if !c.classes.iter().any(|n| n == name) {
        return None;
    }
    let binding = c.env.lookup(name)?;
    if !c
        .resolved
        .scopes
        .iter()
        .flat_map(|s| &s.bindings)
        .any(|b| b.name == name && b.kind == BindingKind::Class && b.span.0 == binding.span.0)
    {
        return None;
    }
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
        if !call.arguments.keywords.is_empty()
            || actuals.len() < min_params.unwrap_or(params.len())
            || (!variadic && actuals.len() > params.len())
        {
            c.wrong_args("<callable variant>", params.len(), actuals.len(), span);
        }
        for (param, actual) in params.iter().zip(&actuals) {
            if !c.is_assignable(param, actual) {
                c.mismatch(param, actual, span);
            }
        }
        returns.push(bind_typevars_and_substitute(params, &actuals, ret));
    }
    Some(Type::union_of(returns))
}
