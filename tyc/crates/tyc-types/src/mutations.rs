//! Checks that writes preserve the declared container contract.
use super::*;

fn check_value(c: &mut Checker, expected: &Type, expr: &Expr) {
    let actual = infer_expr_ctx(c, expr, Some(expected));
    if !c.is_assignable(expected, &actual) {
        c.mismatch(
            expected,
            &actual,
            (
                expr.range().start().to_usize(),
                expr.range().end().to_usize(),
            ),
        );
    }
}

pub(super) fn index(c: &mut Checker, sub: &ruff_python_ast::ExprSubscript, recv: &Type) {
    let Type::Generic(head, args) = recv else {
        return;
    };
    if head == "dict" && args.len() == 2 {
        check_value(c, &args[0], &sub.slice);
    } else if head == "list" && args.len() == 1 {
        if let Expr::Slice(slice) = sub.slice.as_ref() {
            let bound = Type::union_of(vec![Type::Int, Type::None]);
            for expr in [&slice.lower, &slice.upper, &slice.step]
                .into_iter()
                .flatten()
            {
                check_value(c, &bound, expr);
            }
        } else {
            let actual = infer_expr(c, &sub.slice);
            let index_protocol = match &actual {
                Type::Class(name) | Type::Generic(name, _) => {
                    c.find_method(name, "__index__").is_some()
                }
                _ => false,
            };
            if !matches!(&actual, Type::Class(n) if n == "slice" && !c.classes.contains(n))
                && !index_protocol
                && !c.is_assignable(&Type::Int, &actual)
            {
                c.mismatch(
                    &Type::Int,
                    &actual,
                    (
                        sub.slice.range().start().to_usize(),
                        sub.slice.range().end().to_usize(),
                    ),
                );
            }
        }
    }
}

pub(super) fn update(c: &mut Checker, call: &ruff_python_ast::ExprCall) -> Option<Type> {
    let Expr::Attribute(attr) = call.func.as_ref() else {
        return None;
    };
    if attr.attr.as_str() != "update" {
        return None;
    }
    let recv = infer_expr_readonly(c, &attr.value);
    let Type::Generic(head, args) = &recv else {
        return None;
    };
    if head != "dict" || args.len() != 2 {
        return None;
    }
    let _ = infer_expr(c, &attr.value);
    let span = (call.range.start().to_usize(), call.range.end().to_usize());
    if call.arguments.args.len() > 1 {
        c.wrong_args("dict.update", 1, call.arguments.args.len(), span);
    }
    for arg in &call.arguments.args {
        let actual = infer_expr(c, arg);
        let slots = match &actual {
            Type::Generic(h, a)
                if matches!(h.as_str(), "dict" | "Mapping" | "MutableMapping") && a.len() == 2 =>
            {
                Some((a[0].clone(), a[1].clone()))
            }
            _ => match iterable_element_type(&actual) {
                Some(Type::Generic(h, a)) if h == "tuple" && a.len() == 2 => {
                    Some((a[0].clone(), a[1].clone()))
                }
                _ => None,
            },
        };
        if let Some((key, value)) = slots {
            for (expected, actual) in [(&args[0], &key), (&args[1], &value)] {
                if !c.is_assignable(expected, actual) {
                    c.mismatch(
                        expected,
                        actual,
                        (arg.range().start().to_usize(), arg.range().end().to_usize()),
                    );
                }
            }
        }
    }
    for keyword in &call.arguments.keywords {
        if keyword.arg.is_some() {
            if !c.is_assignable(&args[0], &Type::Str) {
                c.mismatch(&args[0], &Type::Str, span);
            }
            check_value(c, &args[1], &keyword.value);
        } else {
            let actual = infer_expr(c, &keyword.value);
            if let Type::Generic(h, a) = &actual {
                if matches!(h.as_str(), "dict" | "Mapping") && a.len() == 2 {
                    for (expected, actual) in args.iter().zip(a) {
                        if !c.is_assignable(expected, actual) {
                            c.mismatch(expected, actual, span);
                        }
                    }
                }
            }
        }
    }
    Some(Type::None)
}

pub(super) fn slice_object(c: &Checker, expr: &Expr) -> bool {
    matches!(infer_expr_readonly(c, expr),Type::Class(n) if n == "slice" && !c.classes.contains(&n))
}
