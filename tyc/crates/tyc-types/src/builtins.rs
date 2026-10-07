//! Call-specific contracts for CPython builtins. Unknown input stays local to
//! the affected overload; fixed result types never become globally unknown.
use super::{iterable_element_type, Type};

#[derive(Clone, Copy)]
enum ResultRule {
    Int,
    Str,
    Float,
    Bool,
    Range,
    Slice,
    Container(&'static str),
    Dict,
    FrozenDict,
    Class(&'static str),
    Same,
    Round,
    Sum,
    Extreme,
    Enumerate,
    Zip,
    Iterator,
}
struct Contract {
    name: &'static str,
    min: usize,
    max: usize,
    keywords: &'static [&'static str],
    result: ResultRule,
}
macro_rules! contract {
    ($n:literal, $min:literal, $max:expr, $kw:expr, $r:expr) => {
        Contract {
            name: $n,
            min: $min,
            max: $max,
            keywords: $kw,
            result: $r,
        }
    };
}
const CONTRACTS: &[Contract] = &[
    contract!("slice", 1, 3, &[], ResultRule::Slice),
    contract!("len", 1, 1, &[], ResultRule::Int),
    contract!("int", 0, 2, &["base"], ResultRule::Int),
    contract!(
        "str",
        0,
        3,
        &["object", "encoding", "errors"],
        ResultRule::Str
    ),
    contract!("float", 0, 1, &[], ResultRule::Float),
    contract!("bool", 0, 1, &[], ResultRule::Bool),
    contract!("list", 0, 1, &[], ResultRule::Container("list")),
    contract!("tuple", 0, 1, &[], ResultRule::Container("tuple_variadic")),
    contract!("set", 0, 1, &[], ResultRule::Container("set")),
    contract!("frozenset", 0, 1, &[], ResultRule::Container("frozenset")),
    contract!("dict", 0, 1, &["*"], ResultRule::Dict),
    // New in Python 3.15 (PEP 814 / PEP 661); `tyc::requires_newer_python`
    // rejects them on older targets.
    contract!("frozendict", 0, 1, &["*"], ResultRule::FrozenDict),
    contract!("sentinel", 1, 1, &[], ResultRule::Class("sentinel")),
    contract!("abs", 1, 1, &[], ResultRule::Same),
    contract!("round", 1, 2, &["number", "ndigits"], ResultRule::Round),
    contract!("sum", 1, 2, &["start"], ResultRule::Sum),
    contract!(
        "min",
        1,
        usize::MAX,
        &["key", "default"],
        ResultRule::Extreme
    ),
    contract!(
        "max",
        1,
        usize::MAX,
        &["key", "default"],
        ResultRule::Extreme
    ),
    contract!(
        "sorted",
        1,
        1,
        &["key", "reverse"],
        ResultRule::Container("list")
    ),
    contract!("reversed", 1, 1, &[], ResultRule::Iterator),
    contract!("iter", 1, 2, &[], ResultRule::Iterator),
    contract!("range", 1, 3, &[], ResultRule::Range),
    contract!(
        "enumerate",
        1,
        2,
        &["iterable", "start"],
        ResultRule::Enumerate
    ),
    contract!("zip", 0, usize::MAX, &["strict"], ResultRule::Zip),
    contract!("repr", 1, 1, &[], ResultRule::Str),
    contract!("input", 0, 1, &[], ResultRule::Str),
    contract!("any", 1, 1, &[], ResultRule::Bool),
    contract!("all", 1, 1, &[], ResultRule::Bool),
    contract!("ord", 1, 1, &[], ResultRule::Int),
    contract!("chr", 1, 1, &[], ResultRule::Str),
    contract!("isinstance", 2, 2, &[], ResultRule::Bool),
    contract!("issubclass", 2, 2, &[], ResultRule::Bool),
];

pub(super) fn contains(name: &str) -> bool {
    CONTRACTS.iter().any(|c| c.name == name)
}
pub(super) fn valid_arity(name: &str, positional: usize, keywords: &[&str]) -> bool {
    let Some(c) = CONTRACTS.iter().find(|c| c.name == name) else {
        return true;
    };
    let positional_keywords = match name {
        "round" => keywords
            .iter()
            .filter(|k| matches!(**k, "number" | "ndigits"))
            .count(),
        "enumerate" => keywords
            .iter()
            .filter(|k| matches!(**k, "iterable" | "start"))
            .count(),
        "str" => keywords.len(),
        "int" | "sum" => {
            usize::from(keywords.contains(&if name == "int" { "base" } else { "start" }))
        }
        _ => 0,
    };
    let total = positional + positional_keywords;
    total >= c.min
        && total <= c.max
        && keywords
            .iter()
            .all(|k| c.keywords.contains(k) || c.keywords.contains(&"*"))
}
pub(super) fn result(name: &str, args: &[Type], keywords: &[(&str, Type)]) -> Option<Type> {
    let c = CONTRACTS.iter().find(|c| c.name == name)?;
    let first = args
        .first()
        .or_else(|| {
            keywords
                .iter()
                .find(|(k, _)| matches!(*k, "number" | "iterable" | "object"))
                .map(|(_, v)| v)
        })
        .cloned()
        .unwrap_or(Type::Unknown);
    let elem = || iterable_element_type(&first).unwrap_or(Type::Unknown);
    let generic = |head: &str, args: Vec<Type>| Type::Generic(head.into(), args);
    Some(match c.result {
        ResultRule::Int => Type::Int,
        ResultRule::Str => Type::Str,
        ResultRule::Float => Type::Float,
        ResultRule::Bool => Type::Bool,
        ResultRule::Slice => Type::Class("slice".into()),
        ResultRule::Range => Type::Class("range".into()),
        ResultRule::Container(head) => generic(head, vec![elem()]),
        ResultRule::Iterator => generic("Iterator", vec![elem()]),
        ResultRule::FrozenDict => match result("dict", args, keywords)? {
            Type::Generic(_, params) => generic("frozendict", params),
            other => other,
        },
        ResultRule::Class(name) => Type::Class(name.into()),
        ResultRule::Dict => {
            if let Type::Generic(head, params) = &first {
                if matches!(
                    head.as_str(),
                    "dict" | "Mapping" | "MutableMapping" | "frozendict"
                ) {
                    return Some(generic("dict", params.clone()));
                }
            }
            let pair = elem();
            let (key, value) = match pair {
                Type::Generic(head, params) if head == "tuple" && params.len() == 2 => {
                    (params[0].clone(), params[1].clone())
                }
                _ => (Type::Unknown, Type::Unknown),
            };
            if keywords.is_empty() {
                generic("dict", vec![key, value])
            } else {
                generic(
                    "dict",
                    vec![
                        Type::Str,
                        Type::union_of(keywords.iter().map(|(_, v)| v.clone()).collect()),
                    ],
                )
            }
        }
        ResultRule::Same => match first {
            Type::Bool => Type::Int,
            Type::Int | Type::Float => first,
            _ => Type::Unknown,
        },
        ResultRule::Round => {
            let digits = args.get(1).or_else(|| {
                keywords
                    .iter()
                    .find(|(k, _)| *k == "ndigits")
                    .map(|(_, v)| v)
            });
            if digits.is_none_or(|t| *t == Type::None) {
                Type::Int
            } else {
                match first {
                    Type::Float => Type::Float,
                    Type::Int | Type::Bool => Type::Int,
                    _ => Type::Unknown,
                }
            }
        }
        ResultRule::Sum => {
            let start = args
                .get(1)
                .or_else(|| keywords.iter().find(|(k, _)| *k == "start").map(|(_, v)| v));
            let element = elem();
            match (element, start) {
                (Type::Bool, _) => Type::Int,
                (Type::Int, Some(Type::Float)) => Type::Float,
                (t, None) => t,
                (t, Some(s)) => Type::union_of(vec![t, s.clone()]),
            }
        }
        ResultRule::Extreme => {
            let value = if args.len() > 1 {
                Type::union_of(args.to_vec())
            } else {
                elem()
            };
            match keywords.iter().find(|(k, _)| *k == "default") {
                Some((_, d)) => Type::union_of(vec![value, d.clone()]),
                None => value,
            }
        }
        ResultRule::Enumerate => {
            generic("Iterator", vec![generic("tuple", vec![Type::Int, elem()])])
        }
        ResultRule::Zip => generic(
            "Iterator",
            vec![generic(
                "tuple",
                args.iter()
                    .map(|t| iterable_element_type(t).unwrap_or(Type::Unknown))
                    .collect(),
            )],
        ),
    })
}

/// Builtins that are classes, not functions. Used as a value (`d[list] =
/// …`, `let t: type = int`, `isinstance(x, zip)`) such a name is the class
/// object, which a plain `Function` type misrepresents: it would not be
/// assignable to `type`.
const BUILTIN_CLASSES: &[&str] = &[
    "bool",
    "bytearray",
    "bytes",
    "classmethod",
    "complex",
    "dict",
    "enumerate",
    "filter",
    "float",
    "frozendict",
    "frozenset",
    "int",
    "list",
    "map",
    "memoryview",
    "object",
    "property",
    "range",
    "reversed",
    "sentinel",
    "set",
    "slice",
    "staticmethod",
    "str",
    "super",
    "tuple",
    "type",
    "zip",
];

pub(super) fn value_type(name: &str) -> Option<Type> {
    // A builtin class used as a value stays untyped, as before builtin
    // contracts existed; a position expecting a `Callable` is served by
    // `callables::constructor` before this is reached.
    if BUILTIN_CLASSES.contains(&name) {
        return None;
    }
    let c = CONTRACTS.iter().find(|c| c.name == name)?;
    let count = if c.max == usize::MAX { c.min } else { c.max };
    let params = vec![Type::Unknown; count];
    Some(Type::Function {
        ret: Box::new(result(name, &params, &[]).unwrap_or(Type::Unknown)),
        params,
        variadic: c.max == usize::MAX,
        min_params: Some(c.min),
    })
}

pub(super) fn argument_type(name: &str, index: usize, keyword: Option<&str>) -> Option<Type> {
    let integer = || Type::Int;
    match (name, index, keyword) {
        ("range", _, _) => Some(integer()),
        ("int", 1, _)
        | ("int", _, Some("base"))
        | ("enumerate", 1, _)
        | ("enumerate", _, Some("start")) => Some(integer()),
        ("round", 1, _) | ("round", _, Some("ndigits")) => {
            Some(Type::union_of(vec![Type::Int, Type::None]))
        }
        _ => None,
    }
}
