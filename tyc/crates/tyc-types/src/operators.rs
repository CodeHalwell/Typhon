//! Result typing shared by arithmetic expressions and augmented assignments.
use super::*;

pub(super) fn power_result(
    c: &Checker,
    left: &Type,
    right: &Type,
    exponent: &Expr,
) -> Option<Type> {
    let left_base = newtype_with_base(c, left)
        .map(|(_, base)| base)
        .unwrap_or_else(|| left.clone());
    let right_base = newtype_with_base(c, right)
        .map(|(_, base)| base)
        .unwrap_or_else(|| right.clone());
    if !matches!(left_base, Type::Int | Type::Bool) || !matches!(right_base, Type::Int | Type::Bool)
    {
        return None;
    }
    let integer_result = if matches!(left, Type::Bool) {
        Type::Int
    } else {
        left.clone()
    };
    fn negative(expr: &Expr) -> Option<bool> {
        match expr {
            Expr::NumberLiteral(n) if matches!(n.value, Number::Int(_)) => Some(false),
            Expr::BooleanLiteral(_) => Some(false),
            Expr::BinOp(b)
                if matches!(b.op, Operator::Pow)
                    && negative(&b.left) == Some(false)
                    && negative(&b.right) == Some(false) =>
            {
                Some(false)
            }

            Expr::UnaryOp(u)
                if matches!(u.op, ruff_python_ast::UnaryOp::USub)
                    && matches!(u.operand.as_ref(),Expr::NumberLiteral(n) if matches!(n.value,Number::Int(_))) =>
            {
                Some(!is_literal_zero(&u.operand))
            }
            Expr::UnaryOp(u) if matches!(u.op, ruff_python_ast::UnaryOp::UAdd) => {
                negative(&u.operand)
            }
            _ => None,
        }
    }
    Some(match negative(exponent) {
        Some(false) => integer_result,
        Some(true) => Type::Float,
        None => Type::union_of(vec![integer_result, Type::Float]),
    })
}

pub(super) fn augmented_result(
    c: &mut Checker,
    left: &Type,
    right: &Type,
    op: Operator,
    rhs: &Expr,
) -> Type {
    if matches!(op, Operator::Pow) {
        if let Some(result) = power_result(c, left, right, rhs) {
            return result;
        }
    }
    if let Type::Class(name) | Type::Generic(name, _) = left {
        if let Some(dunder) = binop_dunder(op) {
            let inplace = format!("__i{}", dunder.trim_start_matches("__"));
            if let Some(sig) = c
                .find_method(name, &inplace)
                .or_else(|| c.find_method(name, dunder))
            {
                let sig = sig.clone();
                if !dunder_accepts(c, &sig, right) {
                    let span = (rhs.range().start().to_usize(), rhs.range().end().to_usize());
                    c.mismatch(&sig.param_types[0], right, span);
                }
                return sig.return_type;
            }
        }
    }
    let lbase = newtype_with_base(c, left)
        .map(|(_, base)| base)
        .unwrap_or_else(|| left.clone());
    let rbase = newtype_with_base(c, right)
        .map(|(_, base)| base)
        .unwrap_or_else(|| right.clone());
    match (&lbase, &rbase) {
        _ if matches!(op, Operator::Div) && is_numeric(&lbase) && is_numeric(&rbase) => Type::Float,
        (Type::Bool, Type::Bool)
            if matches!(op, Operator::BitAnd | Operator::BitOr | Operator::BitXor) =>
        {
            Type::Bool
        }
        (Type::Int | Type::Bool, Type::Int | Type::Bool) => {
            if matches!(left, Type::Class(_)) {
                left.clone()
            } else {
                Type::Int
            }
        }
        (Type::Float, Type::Int | Type::Bool | Type::Float)
        | (Type::Int | Type::Bool, Type::Float) => Type::Float,
        (Type::Str, Type::Str) if matches!(op, Operator::Add) => Type::Str,
        (Type::Bytes, Type::Bytes) if matches!(op, Operator::Add) => Type::Bytes,
        (Type::Str | Type::Bytes, Type::Int | Type::Bool) if matches!(op, Operator::Mult) => {
            lbase.clone()
        }
        (Type::Int | Type::Bool, Type::Str | Type::Bytes) if matches!(op, Operator::Mult) => {
            rbase.clone()
        }
        (Type::Generic(lh, la), Type::Generic(rh, ra))
            if matches!(op, Operator::Add)
                && matches!(lh.as_str(), "tuple" | "tuple_variadic")
                && matches!(rh.as_str(), "tuple" | "tuple_variadic") =>
        {
            let mut elements = la.clone();
            elements.extend(ra.clone());
            if lh == "tuple" && rh == "tuple" {
                Type::Generic("tuple".into(), elements)
            } else {
                Type::Generic("tuple_variadic".into(), vec![Type::union_of(elements)])
            }
        }
        (Type::Generic(head, _), Type::Int | Type::Bool)
            if matches!(op, Operator::Mult)
                && matches!(head.as_str(), "list" | "tuple_variadic") =>
        {
            left.clone()
        }
        (Type::Generic(head, _), _) if head == "list" && matches!(op, Operator::Add) => {
            left.clone()
        }
        _ => Type::Unknown,
    }
}
