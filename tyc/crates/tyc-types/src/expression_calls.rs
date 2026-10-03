//! Expression-level concurrency and Result contracts.
use super::*;

pub(super) fn callback_result(c: &mut Checker, callback: &Expr, params: Vec<Type>) -> Type {
    let contract = Type::Function {
        min_params: Some(params.len()),
        params,
        ret: Box::new(Type::Unknown),
        variadic: false,
    };
    let actual = infer_expr_ctx(c, callback, Some(&contract));
    if !c.is_assignable(&contract, &actual) {
        c.mismatch(
            &contract,
            &actual,
            (
                callback.range().start().to_usize(),
                callback.range().end().to_usize(),
            ),
        );
    }
    match actual {
        Type::Function { ret, .. } => *ret,
        _ => Type::Unknown,
    }
}

pub(super) fn contract(c: &mut Checker, call: &ruff_python_ast::ExprCall) -> Option<Type> {
    let Expr::Attribute(attr) = call.func.as_ref() else {
        return None;
    };
    let name = attr.attr.as_str();
    if !matches!(
        name,
        "TaskGroup"
            | "spawn"
            | "create_task"
            | "gather"
            | "map"
            | "map_err"
            | "and_then"
            | "or_else"
    ) {
        return None;
    }
    let recv = infer_expr_readonly(c, &attr.value);
    let asyncio = matches!(&recv,Type::Module(m) if m=="asyncio")
        || matches!(attr.value.as_ref(),Expr::Name(n) if n.id.as_str()=="asyncio" && c.env.lookup("asyncio").is_none());
    if asyncio && name == "TaskGroup" {
        return Some(Type::Class("asyncio.TaskGroup".into()));
    }
    let spawn = name == "spawn" && task_spawn_callee(call).is_some();
    let create_task = name == "create_task"
        && (asyncio || matches!(&recv,Type::Class(n) if n=="asyncio.TaskGroup"));
    if spawn || create_task || (asyncio && name == "gather") {
        if spawn
            && c.current_return.is_none()
            && c.async_enclosing_depth == 0
            && c.unsafe_depth == 0
        {
            let span = (call.range.start().to_usize(), call.range.end().to_usize());
            c.diagnostics.push_error(TycError::go_outside_async(
                task_spawn_callee(call).unwrap(),
                c.path.clone(),
                c.source,
                span.0,
                span.1.saturating_sub(span.0).max(1),
            ));
        }
        c.inside_await += 1;
        let mut results: Vec<Type> = call
            .arguments
            .args
            .iter()
            .map(|arg| {
                let ty = infer_expr(c, arg);
                unwrap_awaitable(&ty, &c.classes).unwrap_or(Type::Unknown)
            })
            .collect();
        c.inside_await -= 1;
        if spawn || create_task {
            if call.arguments.args.len() != 1 {
                c.wrong_args(
                    name,
                    1,
                    call.arguments.args.len(),
                    (call.range.start().to_usize(), call.range.end().to_usize()),
                );
            }
            return Some(Type::Generic(
                "Task".into(),
                vec![results.into_iter().next().unwrap_or(Type::Unknown)],
            ));
        }
        let best_effort = call.arguments.keywords.iter().any(|kw| {
            kw.arg
                .as_ref()
                .is_some_and(|n| n.as_str() == "return_exceptions")
                && !matches!(kw.value,Expr::BooleanLiteral(ref b) if !b.value)
        });
        if best_effort {
            for result in &mut results {
                *result = Type::union_of(vec![result.clone(), Type::Class("Exception".into())]);
            }
        }
        return Some(Type::Generic(
            "Coroutine".into(),
            vec![Type::Generic("tuple".into(), results)],
        ));
    }
    let recv = if matches!(&recv,Type::Generic(h,_) if matches!(h.as_str(),"Ok"|"Err"|"Result")) {
        infer_expr(c, &attr.value)
    } else {
        recv
    };
    let Type::Generic(head, args) = &recv else {
        return None;
    };
    let (ok, error) = match (head.as_str(), args.as_slice()) {
        ("Result", [t, e]) => (t.clone(), e.clone()),
        ("Ok", [t]) => (t.clone(), Type::Unknown),
        ("Err", [e]) => (Type::Unknown, e.clone()),
        _ => return None,
    };
    if !matches!(name, "map" | "map_err" | "and_then" | "or_else") {
        return None;
    }
    if call.arguments.args.len() != 1 || !call.arguments.keywords.is_empty() {
        c.wrong_args(
            name,
            1,
            call.arguments.args.len(),
            (call.range.start().to_usize(), call.range.end().to_usize()),
        );
        return Some(Type::Unknown);
    }
    let input = if matches!(name, "map_err" | "or_else") {
        error.clone()
    } else {
        ok.clone()
    };
    let mapped = callback_result(c, &call.arguments.args[0], vec![input]);
    match (head.as_str(), name) {
        ("Err", "map" | "and_then") | ("Ok", "map_err" | "or_else") => Some(recv),
        ("Ok", "map") => Some(Type::Generic("Ok".into(), vec![mapped])),
        ("Err", "map_err") => Some(Type::Generic("Err".into(), vec![mapped])),
        (_, "map") => Some(Type::Generic("Result".into(), vec![mapped, error])),
        (_, "map_err") => Some(Type::Generic("Result".into(), vec![ok, mapped])),
        (_, "and_then" | "or_else") => {
            let (t, e) = match &mapped {
                Type::Generic(h, a) if h == "Result" && a.len() == 2 => {
                    (a[0].clone(), a[1].clone())
                }
                Type::Generic(h, a) if h == "Ok" && a.len() == 1 => (a[0].clone(), error.clone()),
                Type::Generic(h, a) if h == "Err" && a.len() == 1 => (ok.clone(), a[0].clone()),
                Type::Unknown | Type::Any => (Type::Unknown, Type::Unknown),
                _ => {
                    c.mismatch_with(
                        "a Result-returning callable".into(),
                        mapped.display(),
                        (call.range.start().to_usize(), call.range.end().to_usize()),
                    );
                    return Some(Type::Unknown);
                }
            };
            Some(Type::Generic(
                "Result".into(),
                if name == "and_then" {
                    vec![t, Type::union_of(vec![error, e])]
                } else {
                    vec![Type::union_of(vec![ok, t]), e]
                },
            ))
        }
        _ => None,
    }
}
