//! Named field defaults the desugar copies per instance (W7-07).
//!
//! A dataclass field whose default is a plain or dotted *name* —
//! `items: list[int] = BASE` — lowers to
//! `field(default_factory=lambda: list(BASE))`, so every instance holds its
//! own shallow `list` / `dict` / `set` copy of whatever `BASE` is at runtime.
//! Two stages need the same answer about which defaults that applies to:
//!
//! * `tyc-desugar` decides which defaults it *rewrites*.
//! * `tyc-types` decides which defaults it may check as the *copy* rather
//!   than as the named value itself (`items: list[int] = T` with `T` a tuple
//!   is a real list at runtime).
//!
//! If the two disagreed, the checker would either reject a correct program
//! or accept a field whose default is stored as written and crashes. So the
//! field-level rules live here, in the lowest crate both already depend on.
//! The class-level gate (only classes that receive `@dataclass` are
//! rewritten) stays with each consumer, since it reads per-crate markers.

use ruff_python_ast::{Expr, Stmt};
use std::collections::{HashMap, HashSet};

/// `list` / `dict` / `set` when `annotation` is (a subscript of) one of
/// them, or its `typing` alias.
pub fn mutable_builtin_of_annotation(annotation: &Expr) -> Option<&'static str> {
    let name = match annotation {
        Expr::Subscript(s) => return mutable_builtin_of_annotation(&s.value),
        Expr::Name(n) => n.id.as_str(),
        Expr::Attribute(a) => a.attr.as_str(),
        _ => return None,
    };
    match name {
        "list" | "List" => Some("list"),
        "dict" | "Dict" => Some("dict"),
        "set" | "Set" => Some("set"),
        _ => None,
    }
}

/// `list` / `dict` / `set` when `value` evidently builds one.
fn mutable_builtin_of_value(value: &Expr) -> Option<&'static str> {
    match value {
        Expr::List(_) | Expr::ListComp(_) => Some("list"),
        Expr::Dict(_) | Expr::DictComp(_) => Some("dict"),
        Expr::Set(_) | Expr::SetComp(_) => Some("set"),
        Expr::Call(c) => match c.func.as_ref() {
            Expr::Name(n) => match n.id.as_str() {
                "list" => Some("list"),
                "dict" => Some("dict"),
                "set" => Some("set"),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// Module-level names bound to an evident `list` / `dict` / `set`, by their
/// annotation or their value. A name bound twice to different kinds is left
/// out.
pub fn collect_module_mutable_names(body: &[Stmt]) -> HashMap<String, &'static str> {
    let mut out: HashMap<String, &'static str> = HashMap::new();
    let mut conflicting: HashSet<String> = HashSet::new();
    for stmt in body {
        let (name, kind) = match stmt {
            Stmt::AnnAssign(a) => {
                let Expr::Name(n) = a.target.as_ref() else {
                    continue;
                };
                let kind = mutable_builtin_of_annotation(&a.annotation)
                    .or_else(|| a.value.as_deref().and_then(mutable_builtin_of_value));
                (n.id.as_str(), kind)
            }
            Stmt::Assign(a) => {
                let [Expr::Name(n)] = a.targets.as_slice() else {
                    continue;
                };
                (n.id.as_str(), mutable_builtin_of_value(&a.value))
            }
            _ => continue,
        };
        match (kind, out.get(name)) {
            (Some(k), None) => {
                out.insert(name.to_owned(), k);
            }
            (Some(k), Some(prev)) if *prev == k => {}
            _ => {
                conflicting.insert(name.to_owned());
            }
        }
    }
    out.retain(|name, _| !conflicting.contains(name));
    out
}

/// For a field default that is a plain or dotted name (`BASE`,
/// `config.DEFAULTS`) whose value is evidently a `list` / `dict` / `set` —
/// by the field's own annotation, or by a module-level binding of the name —
/// the builtin to copy it with.
pub fn named_mutable_default_kind(
    value: &Expr,
    annotation: &Expr,
    module_mutable_names: &HashMap<String, &'static str>,
) -> Option<&'static str> {
    fn is_dotted_name(e: &Expr) -> bool {
        match e {
            Expr::Name(_) => true,
            Expr::Attribute(a) => is_dotted_name(&a.value),
            _ => false,
        }
    }
    if !is_dotted_name(value) {
        return None;
    }
    mutable_builtin_of_annotation(annotation).or_else(|| match value {
        Expr::Name(n) => module_mutable_names.get(n.id.as_str()).copied(),
        _ => None,
    })
}

/// Names a class body itself binds. A lambda defined in the body cannot see
/// them, so a default that mentions one is never wrapped in a factory.
pub fn class_body_names(body: &[Stmt]) -> HashSet<String> {
    body.iter()
        .filter_map(|s| match s {
            Stmt::AnnAssign(a) => match a.target.as_ref() {
                Expr::Name(n) => Some(n.id.as_str().to_owned()),
                _ => None,
            },
            Stmt::Assign(a) => a.targets.iter().find_map(|t| match t {
                Expr::Name(n) => Some(n.id.as_str().to_owned()),
                _ => None,
            }),
            Stmt::FunctionDef(f) => Some(f.name.as_str().to_owned()),
            Stmt::ClassDef(c) => Some(c.name.as_str().to_owned()),
            _ => None,
        })
        .collect()
}

/// True when `ann` is `ClassVar[...]`, in any spelling the language accepts —
/// bare, `typing.ClassVar`, or an aliased module (`t.ClassVar`). A `ClassVar`
/// default is a class attribute, never copied.
pub fn is_classvar_annotation(ann: &Expr) -> bool {
    let head = match ann {
        Expr::Subscript(s) => s.value.as_ref(),
        other => other,
    };
    match head {
        Expr::Name(n) => n.id.as_str() == "ClassVar",
        Expr::Attribute(a) => a.attr.as_str() == "ClassVar",
        _ => false,
    }
}
