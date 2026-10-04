//! Result typing shared by arithmetic expressions and augmented assignments.
use super::*;
use std::cmp::Ordering;

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
    /// The sign of a constant integer expression — `Less`, `Equal` (zero)
    /// or `Greater` — or `None` when the syntax does not prove it.
    fn sign(expr: &Expr) -> Option<Ordering> {
        match expr {
            Expr::NumberLiteral(n) if matches!(n.value, Number::Int(_)) => {
                Some(if is_literal_zero(expr) {
                    Ordering::Equal
                } else {
                    Ordering::Greater
                })
            }
            Expr::BooleanLiteral(b) => Some(if b.value {
                Ordering::Greater
            } else {
                Ordering::Equal
            }),
            // `a ** b` over a non-negative base and exponent: `0 ** 0 == 1`,
            // `0 ** b == 0` for `b > 0`, positive otherwise.
            Expr::BinOp(b) if matches!(b.op, Operator::Pow) => {
                match (sign(&b.left)?, sign(&b.right)?) {
                    (Ordering::Equal, Ordering::Greater) => Some(Ordering::Equal),
                    (Ordering::Equal | Ordering::Greater, Ordering::Equal | Ordering::Greater) => {
                        Some(Ordering::Greater)
                    }
                    _ => None,
                }
            }
            Expr::UnaryOp(u) => match u.op {
                ruff_python_ast::UnaryOp::USub => sign(&u.operand).map(Ordering::reverse),
                ruff_python_ast::UnaryOp::UAdd => sign(&u.operand),
                _ => None,
            },
            _ => None,
        }
    }
    // Only a provably negative exponent makes the result a float. An
    // exponent of unknown sign keeps the integer type: rejecting
    // `def power(base: int, exp: int) -> int: return base ** exp` would
    // narrow programs that only ever pass non-negative exponents and run
    // correctly.
    Some(match sign(exponent) {
        Some(Ordering::Less) => Type::Float,
        Some(Ordering::Equal | Ordering::Greater) | None => integer_result,
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
