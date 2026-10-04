//! `EXPR as! TYPE` on the VM: the checked-boundary-cast target table
//! (`docs/language.md`, "Checked boundary casts"), as the generated
//! `typhon_runtime/cast.py` applies it on the compile path.
//!
//! The compiled program evaluates `TYPE` into a `typing` object and walks
//! it; the VM erases most typing objects to identity stand-ins, so it reads
//! the target's AST instead and resolves the two names that carry a
//! definition of their own:
//!
//! * a PEP 695 `type` alias, through the registry of alias definitions
//!   ([`AliasDef`]) — its right-hand side, with the alias's parameters bound
//!   to the use site's arguments (`Pair[int]`);
//! * a `newtype`, through the base expression recorded when the `NewType`
//!   object was created ([`NewTypeDef`]).
//!
//! Matching, refusals and the failure message follow `cast.py` line by line:
//! a mismatch is `TypeError: as! cast failed: value of type X does not match
//! <str(TYPE)>`, and a target with no runtime shape (an unbound parameter, a
//! parameterised user class, `Callable[…]`, an iterator contract) is a
//! `TypeError: as! cannot check …`.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use ruff_python_ast::{self as ast, Expr, Operator};

use super::{class_is_protocol, protocol_members, Interpreter};
use crate::env::EnvRef;
use crate::error::{type_error, Unwind};
use crate::value::{HashKey, Value};

/// A `type NAME[PARAMS] = VALUE` statement, kept so a cast can expand it.
pub(crate) struct AliasDef {
    pub name: String,
    pub params: Vec<String>,
    pub value: Rc<Expr>,
    /// The module scope the alias was defined in (its right-hand side's
    /// names resolve there).
    pub module: EnvRef,
}

/// The base of a `NewType(name, base)` object.
pub(crate) struct NewTypeDef {
    pub base: Rc<Expr>,
    pub env: EnvRef,
}

/// A type parameter's argument while an alias is expanded, or `None` for a
/// parameter the cast left unbound (`x as! Pair`).
#[derive(Clone)]
struct Arg {
    expr: Rc<Expr>,
    env: EnvRef,
    bindings: Rc<Bindings>,
}

type Bindings = HashMap<String, Option<Arg>>;

/// The container shapes the table checks element by element.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    List,
    Set,
    FrozenSet,
    Sequence,
    Collection,
    AbstractSet,
    Dict,
    Mapping,
    MutableMapping,
    Tuple,
}

/// What a subscript base or a bare name stands for in a target.
enum Origin {
    Kind(Kind),
    Optional,
    Union,
    Literal,
    Annotated,
    /// A generic origin with no runtime check (`Callable`, `Iterator`, a
    /// user generic class, …): refused.
    Refused,
}

/// `typing` names whose bare form is a generic alias CPython's `cast.py`
/// refuses (`get_origin` is set, and it is not a shape the table checks).
const REFUSED_TYPING: &[&str] = &[
    "Callable",
    "Iterable",
    "Iterator",
    "Generator",
    "AsyncIterable",
    "AsyncIterator",
    "AsyncGenerator",
    "Awaitable",
    "Coroutine",
    "Type",
    "MutableSequence",
    "MutableSet",
    "Deque",
    "DefaultDict",
    "OrderedDict",
    "Counter",
    "ChainMap",
    "Container",
    "Hashable",
    "Sized",
    "Reversible",
    "KeysView",
    "ValuesView",
    "ItemsView",
    "MappingView",
    "ContextManager",
    "AsyncContextManager",
    "Pattern",
    "Match",
];

fn kind_of(name: &str) -> Option<Kind> {
    Some(match name {
        "list" | "List" => Kind::List,
        "set" | "Set" => Kind::Set,
        "frozenset" | "FrozenSet" => Kind::FrozenSet,
        "Sequence" => Kind::Sequence,
        "Collection" => Kind::Collection,
        "AbstractSet" => Kind::AbstractSet,
        "dict" | "Dict" => Kind::Dict,
        "Mapping" => Kind::Mapping,
        "MutableMapping" => Kind::MutableMapping,
        "tuple" | "Tuple" => Kind::Tuple,
        _ => return None,
    })
}

/// The `typing` spelling of a container kind's generic form, for messages.
fn typing_prefix(name: &str) -> &'static str {
    match name {
        "list" | "set" | "frozenset" | "dict" | "tuple" | "type" => "",
        _ => "typing.",
    }
}

/// The last segment of a dotted name expression (`typing.List` → `List`).
fn base_name(e: &Expr) -> Option<&str> {
    match e {
        Expr::Name(n) => Some(n.id.as_str()),
        Expr::Attribute(a) => Some(a.attr.as_str()),
        _ => None,
    }
}

fn args_of(slice: &Expr) -> Vec<&Expr> {
    match slice {
        Expr::Tuple(t) => t.elts.iter().collect(),
        other => vec![other],
    }
}

/// A value's address when it is a container a cyclic structure can pass
/// through again — the `id(value)` half of `cast.py`'s recursion guard.
fn container_id(v: &Value) -> Option<usize> {
    Some(match v {
        Value::List(l) => Rc::as_ptr(l) as *const () as usize,
        Value::Tuple(t) => Rc::as_ptr(t) as *const () as usize,
        Value::Dict(d) => Rc::as_ptr(d) as *const () as usize,
        Value::Set(s) => Rc::as_ptr(s) as *const () as usize,
        Value::Instance(i) => Rc::as_ptr(i) as *const () as usize,
        _ => return None,
    })
}

/// The identity of a binding, for telling an alias's own binding from a
/// same-named value.
fn binding_id(v: &Value) -> Option<usize> {
    Some(match v {
        Value::Str(s) => Rc::as_ptr(s) as *const () as usize,
        Value::Class(c) => Rc::as_ptr(c) as *const () as usize,
        Value::Native(n) => Rc::as_ptr(n) as *const () as usize,
        other => return container_id(other),
    })
}

fn by_name(name: &str) -> Value {
    Value::Str(Rc::new(name.to_owned()))
}

impl Interpreter {
    /// Record a `type` statement for later casts (re-running the statement
    /// replaces the earlier definition from the same module).
    pub(crate) fn register_alias(&mut self, ta: &ast::StmtTypeAlias, env: &EnvRef) {
        let Expr::Name(n) = ta.name.as_ref() else {
            return;
        };
        let module = env.module_scope();
        let params = ta
            .type_params
            .as_deref()
            .map(|tp| tp.iter().map(|p| p.name().as_str().to_owned()).collect())
            .unwrap_or_default();
        let def = Rc::new(AliasDef {
            name: n.id.as_str().to_owned(),
            params,
            value: Rc::new((*ta.value).clone()),
            module: module.clone(),
        });
        let defs = self.alias_defs.entry(def.name.clone()).or_default();
        defs.retain(|d| !Rc::ptr_eq(&d.module, &module));
        defs.push(def);
    }

    /// The alias definition `name` (as written at the cast) refers to, if
    /// the value it resolves to is that alias's own binding.
    fn alias_named(&self, name: &str, resolved: &Value) -> Option<Rc<AliasDef>> {
        let want = binding_id(resolved)?;
        self.alias_defs.get(name)?.iter().find_map(|d| {
            let bound = d.module.get_own(&d.name)?;
            (binding_id(&bound) == Some(want)).then(|| d.clone())
        })
    }

    /// `EXPR as! TYPE`: `value` itself when it matches, else `TypeError`.
    pub(crate) fn checked_cast(
        &mut self,
        value: Value,
        tp: &Expr,
        env: &EnvRef,
    ) -> Result<Value, Unwind> {
        let bindings = Rc::new(Bindings::new());
        if self.cast_matches(&value, tp, env, &bindings, &mut HashSet::new())? {
            return Ok(value);
        }
        let type_name = match &value {
            Value::Class(_) => "type".to_owned(),
            Value::Native(n) if is_builtin_type(n.name) => "type".to_owned(),
            other => other.type_display_name(),
        };
        let shown = self.cast_display(tp, env, &bindings)?;
        Err(type_error(format!(
            "as! cast failed: value of type {type_name} does not match {shown}"
        )))
    }

    fn cast_matches(
        &mut self,
        value: &Value,
        tp: &Expr,
        env: &EnvRef,
        b: &Rc<Bindings>,
        active: &mut HashSet<(usize, usize)>,
    ) -> Result<bool, Unwind> {
        // A type parameter of the alias being expanded.
        if let Expr::Name(n) = tp {
            if let Some(slot) = b.get(n.id.as_str()) {
                return match slot.clone() {
                    Some(arg) => self.cast_matches(value, &arg.expr, &arg.env, &arg.bindings, active),
                    None => Err(type_error(format!(
                        "as! cannot check unbound type parameter {}",
                        n.id.as_str()
                    ))),
                };
            }
        }
        if let Some(name) = base_name(tp) {
            if matches!(name, "Any" | "object") {
                return Ok(true);
            }
        }
        // Alias recursion over a cyclic value must terminate: a (value,
        // target) pair is active only while its branch is being checked.
        let pair = container_id(value).map(|v| (v, tp as *const Expr as usize));
        if let Some(pair) = pair {
            if !active.insert(pair) {
                return Ok(false);
            }
        }
        let result = self.cast_matches_inner(value, tp, env, b, active);
        if let Some(pair) = pair {
            active.remove(&pair);
        }
        result
    }

    fn cast_matches_inner(
        &mut self,
        value: &Value,
        tp: &Expr,
        env: &EnvRef,
        b: &Rc<Bindings>,
        active: &mut HashSet<(usize, usize)>,
    ) -> Result<bool, Unwind> {
        match tp {
            Expr::NoneLiteral(_) => Ok(matches!(value, Value::None)),
            Expr::BinOp(op) if matches!(op.op, Operator::BitOr) => {
                Ok(self.cast_matches(value, &op.left, env, b, active)?
                    || self.cast_matches(value, &op.right, env, b, active)?)
            }
            Expr::Subscript(s) => self.cast_matches_subscript(value, s, env, b, active),
            Expr::Name(_) | Expr::Attribute(_) => {
                let name = base_name(tp).unwrap_or_default().to_owned();
                match name.as_str() {
                    "None" if matches!(tp, Expr::Name(_)) => {
                        return Ok(matches!(value, Value::None))
                    }
                    "float" => {
                        return Ok(matches!(
                            value,
                            Value::Int(_) | Value::Bool(_) | Value::FloatData(_)
                        ))
                    }
                    "complex" => {
                        return Ok(matches!(
                            value,
                            Value::Int(_) | Value::Bool(_) | Value::FloatData(_) | Value::Complex(..)
                        ))
                    }
                    _ => {}
                }
                let target = self.eval_expr(tp, env)?;
                if let Some(def) = self.alias_named(&name, &target) {
                    return self.cast_matches_alias(value, &def, &[], env, b, active);
                }
                if let Some(kind) = kind_of(&name) {
                    if !matches!(target, Value::Class(_)) {
                        return self.cast_kind(value, kind, &[], env, b, active);
                    }
                }
                if REFUSED_TYPING.contains(&name.as_str()) && matches!(target, Value::Native(_)) {
                    return Err(type_error(format!(
                        "as! cannot check parameterised target typing.{name}"
                    )));
                }
                self.cast_matches_value(value, &target, tp, env, b)
            }
            Expr::StringLiteral(s) => Err(type_error(format!(
                "as! cannot check target descriptor {}",
                s.value.to_str()
            ))),
            other => {
                let target = self.eval_expr(other, env)?;
                self.cast_matches_value(value, &target, other, env, b)
            }
        }
    }

    /// A target that has been evaluated to an ordinary runtime value: a
    /// class, a builtin type, a newtype, a protocol (an `interface`).
    fn cast_matches_value(
        &mut self,
        value: &Value,
        target: &Value,
        tp: &Expr,
        env: &EnvRef,
        b: &Rc<Bindings>,
    ) -> Result<bool, Unwind> {
        match target {
            Value::None => return Ok(matches!(value, Value::None)),
            Value::Instance(inst) if inst.class.name == "NewType" => {
                let key = Rc::as_ptr(inst) as *const () as usize;
                if let Some(def) = self.newtype_defs.get(&key) {
                    let (base, base_env) = (def.base.clone(), def.env.clone());
                    let none = Rc::new(Bindings::new());
                    return self.cast_matches(value, &base, &base_env, &none, &mut HashSet::new());
                }
                let supertype = self.get_attr(target, "__supertype__")?;
                return self.cast_matches_value(value, &supertype, tp, env, b);
            }
            Value::Class(cls) if class_is_protocol(cls) => {
                // An interface: shallow member presence.
                let names = protocol_members(cls);
                return Ok(names.iter().all(|m| self.get_attr(value, m).is_ok()));
            }
            Value::Class(_) => return Ok(crate::builtins::is_instance_of(value, target)),
            Value::Native(n) => {
                if matches!(n.name, "Any" | "object") {
                    return Ok(true);
                }
                if is_builtin_type(n.name) {
                    return Ok(crate::builtins::is_instance_of(value, target));
                }
            }
            _ => {}
        }
        Err(type_error(format!(
            "as! cannot check target descriptor {}",
            self.cast_display(tp, env, b)?
        )))
    }

    fn cast_matches_alias(
        &mut self,
        value: &Value,
        def: &Rc<AliasDef>,
        args: &[&Expr],
        env: &EnvRef,
        b: &Rc<Bindings>,
        active: &mut HashSet<(usize, usize)>,
    ) -> Result<bool, Unwind> {
        let mut inner = Bindings::new();
        if !args.is_empty() {
            if args.len() != def.params.len() {
                return Err(type_error(format!(
                    "as! requires all alias arguments for {}",
                    def.name
                )));
            }
            for (p, a) in def.params.iter().zip(args) {
                inner.insert(
                    p.clone(),
                    Some(Arg {
                        expr: Rc::new((*a).clone()),
                        env: env.clone(),
                        bindings: b.clone(),
                    }),
                );
            }
        } else {
            for p in &def.params {
                inner.insert(p.clone(), None);
            }
        }
        let body = def.value.clone();
        let module = def.module.clone();
        self.cast_matches(value, &body, &module, &Rc::new(inner), active)
    }

    fn cast_matches_subscript(
        &mut self,
        value: &Value,
        s: &ast::ExprSubscript,
        env: &EnvRef,
        b: &Rc<Bindings>,
        active: &mut HashSet<(usize, usize)>,
    ) -> Result<bool, Unwind> {
        let args = args_of(&s.slice);
        match self.cast_origin(&s.value, env)? {
            (Origin::Optional, _) => Ok(matches!(value, Value::None)
                || match args.first() {
                    Some(a) => self.cast_matches(value, a, env, b, active)?,
                    None => true,
                }),
            (Origin::Union, _) => {
                for a in &args {
                    if self.cast_matches(value, a, env, b, active)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            (Origin::Annotated, _) => match args.first() {
                Some(a) => self.cast_matches(value, a, env, b, active),
                None => Ok(true),
            },
            (Origin::Literal, _) => {
                for a in &args {
                    let lit = self.eval_expr(a, env)?;
                    if same_exact_type(value, &lit) && self.values_equal(value, &lit)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            (Origin::Kind(kind), _) => self.cast_kind(value, kind, &args, env, b, active),
            (Origin::Refused, Some(def)) => self.cast_matches_alias(value, &def, &args, env, b, active),
            (Origin::Refused, None) => Err(type_error(format!(
                "as! cannot check parameterised target {}",
                self.cast_display(&Expr::Subscript(s.clone()), env, b)?
            ))),
        }
    }

    /// What a subscript's base denotes, with the alias definition when it
    /// is a generic `type` alias.
    fn cast_origin(
        &mut self,
        base: &Expr,
        env: &EnvRef,
    ) -> Result<(Origin, Option<Rc<AliasDef>>), Unwind> {
        let Some(name) = base_name(base).map(str::to_owned) else {
            return Ok((Origin::Refused, None));
        };
        let resolved = self.eval_expr(base, env).ok();
        if let Some(v) = &resolved {
            if let Some(def) = self.alias_named(&name, v) {
                return Ok((Origin::Refused, Some(def)));
            }
            // A user class named like a typing form is still a user class.
            if matches!(v, Value::Class(_)) {
                return Ok((Origin::Refused, None));
            }
        }
        Ok((
            match name.as_str() {
                "Optional" => Origin::Optional,
                "Union" => Origin::Union,
                "Literal" => Origin::Literal,
                "Annotated" => Origin::Annotated,
                other => kind_of(other).map_or(Origin::Refused, Origin::Kind),
            },
            None,
        ))
    }

    /// `isinstance(value, origin)` for a container kind, then every element
    /// (or key and value) against the arguments.
    fn cast_kind(
        &mut self,
        value: &Value,
        kind: Kind,
        args: &[&Expr],
        env: &EnvRef,
        b: &Rc<Bindings>,
        active: &mut HashSet<(usize, usize)>,
    ) -> Result<bool, Unwind> {
        if !self.is_kind(value, kind)? {
            return Ok(false);
        }
        if args.is_empty() {
            return Ok(true);
        }
        match kind {
            Kind::Dict | Kind::Mapping | Kind::MutableMapping => {
                let (Some(k_tp), Some(v_tp)) = (args.first(), args.get(1)) else {
                    return Ok(true);
                };
                for (k, v) in self.mapping_items(value)? {
                    if !self.cast_matches(&k, k_tp, env, b, active)?
                        || !self.cast_matches(&v, v_tp, env, b, active)?
                    {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Kind::Tuple => {
                let items = self.cast_items(value)?;
                if args.len() == 2 && matches!(args[1], Expr::EllipsisLiteral(_)) {
                    for item in &items {
                        if !self.cast_matches(item, args[0], env, b, active)? {
                            return Ok(false);
                        }
                    }
                    return Ok(true);
                }
                // `tuple[()]` is the empty tuple.
                let args: Vec<&Expr> = match args {
                    [Expr::Tuple(t)] if t.elts.is_empty() => Vec::new(),
                    other => other.to_vec(),
                };
                if args.len() != items.len() {
                    return Ok(false);
                }
                for (item, a) in items.iter().zip(args) {
                    if !self.cast_matches(item, a, env, b, active)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            _ => {
                for item in self.cast_items(value)? {
                    if !self.cast_matches(&item, args[0], env, b, active)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
        }
    }

    fn is_kind(&mut self, value: &Value, kind: Kind) -> Result<bool, Unwind> {
        let builtin = |name: &str| crate::builtins::is_instance_of(value, &by_name(name));
        // A user subclass of a `collections.abc` class (or of a shim such as
        // `deque`) — by name through its MRO and builtin bases.
        let abc = |name: &str| matches!(value, Value::Instance(_)) && builtin(name);
        Ok(match kind {
            Kind::List => builtin("list"),
            Kind::Set => builtin("set"),
            Kind::FrozenSet => builtin("frozenset"),
            Kind::Tuple => builtin("tuple"),
            Kind::Dict => builtin("dict"),
            Kind::Sequence => {
                matches!(value, Value::Str(_) | Value::Bytes(_) | Value::Range { .. })
                    || builtin("list")
                    || builtin("tuple")
                    || ["Sequence", "MutableSequence", "deque", "bytearray", "UserList", "UserString"]
                        .iter()
                        .any(|n| abc(n))
            }
            Kind::AbstractSet => {
                builtin("set")
                    || builtin("frozenset")
                    || matches!(
                        value,
                        Value::DictView {
                            kind: crate::value::DictViewKind::Keys
                                | crate::value::DictViewKind::Items,
                            ..
                        }
                    )
                    || ["Set", "MutableSet", "AbstractSet"].iter().any(|n| abc(n))
            }
            Kind::Mapping => {
                matches!(value, Value::Dict(_))
                    || ["Mapping", "MutableMapping", "dict", "UserDict"]
                        .iter()
                        .any(|n| abc(n))
            }
            Kind::MutableMapping => {
                builtin("dict") || ["MutableMapping", "UserDict"].iter().any(|n| abc(n))
            }
            Kind::Collection => {
                if self.is_kind(value, Kind::Sequence)?
                    || self.is_kind(value, Kind::AbstractSet)?
                    || self.is_kind(value, Kind::Mapping)?
                    || matches!(value, Value::DictView { .. })
                {
                    true
                } else if let Value::Instance(inst) = value {
                    // `Collection.__subclasshook__`: the three methods.
                    let class = inst.class.clone();
                    ["__len__", "__iter__", "__contains__"]
                        .iter()
                        .all(|m| self.find_method(&class, m).is_some())
                } else {
                    false
                }
            }
        })
    }

    /// The elements `for item in value` yields.
    fn cast_items(&mut self, value: &Value) -> Result<Vec<Value>, Unwind> {
        Ok(match value {
            Value::List(l) => l.borrow().clone(),
            Value::Tuple(t) => t.as_ref().clone(),
            Value::Set(s) => s.borrow().iter().cloned().map(HashKey::into_value).collect(),
            _ => {
                let it = self.make_iter(value.clone())?;
                let mut out = Vec::new();
                while let Some(v) = self.iter_next(&it)? {
                    out.push(v);
                }
                out
            }
        })
    }

    /// `value.items()`.
    fn mapping_items(&mut self, value: &Value) -> Result<Vec<(Value, Value)>, Unwind> {
        if let Value::Dict(d) = value {
            return Ok(d
                .borrow()
                .iter()
                .map(|(k, v)| (k.clone().into_value(), v.clone()))
                .collect());
        }
        let items = self.get_attr(value, "items")?;
        let items = self.call_value(items, Vec::new(), &[])?;
        let mut out = Vec::new();
        for pair in self.cast_items(&items)? {
            if let Value::Tuple(t) = &pair {
                if let [k, v] = t.as_slice() {
                    out.push((k.clone(), v.clone()));
                }
            }
        }
        Ok(out)
    }

    // ── str(TYPE): the target as `cast.py`'s message prints it ───────────

    /// `str(tp)`: a class prints as `<class 'int'>`; anything else as
    /// `typing` renders it (`list[int]`, `typing.Sequence[int]`, `Pair[int]`,
    /// `__main__.UserId`, `int | None`).
    fn cast_display(
        &mut self,
        tp: &Expr,
        env: &EnvRef,
        b: &Rc<Bindings>,
    ) -> Result<String, Unwind> {
        match tp {
            Expr::Name(_) | Expr::Attribute(_) => {
                if let Expr::Name(n) = tp {
                    if let Some(Some(arg)) = b.get(n.id.as_str()).cloned() {
                        return self.cast_display(&arg.expr, &arg.env, &arg.bindings);
                    }
                }
                let name = base_name(tp).unwrap_or_default().to_owned();
                if name == "None" {
                    return Ok("None".to_owned());
                }
                match self.eval_expr(tp, env) {
                    Ok(Value::Class(c)) if self.alias_named(&name, &Value::Class(c.clone())).is_none() => {
                        Ok(format!("<class '{}'>", class_qualified(&c)))
                    }
                    Ok(Value::Native(n)) if is_builtin_type(n.name) => {
                        Ok(format!("<class '{}'>", n.name))
                    }
                    _ => self.type_repr(tp, env, b),
                }
            }
            other => self.type_repr(other, env, b),
        }
    }

    /// `typing._type_repr`: how a type prints inside a generic's brackets.
    fn type_repr(&mut self, tp: &Expr, env: &EnvRef, b: &Rc<Bindings>) -> Result<String, Unwind> {
        Ok(match tp {
            Expr::NoneLiteral(_) => "None".to_owned(),
            Expr::EllipsisLiteral(_) => "...".to_owned(),
            Expr::StringLiteral(s) => s.value.to_str().to_owned(),
            Expr::List(l) => {
                let parts = l
                    .elts
                    .iter()
                    .map(|e| self.type_repr(e, env, b))
                    .collect::<Result<Vec<_>, _>>()?;
                format!("[{}]", parts.join(", "))
            }
            Expr::Tuple(t) if t.elts.is_empty() => "()".to_owned(),
            Expr::BinOp(op) if matches!(op.op, Operator::BitOr) => format!(
                "{} | {}",
                self.type_repr(&op.left, env, b)?,
                self.type_repr(&op.right, env, b)?
            ),
            Expr::Name(_) | Expr::Attribute(_) => {
                if let Expr::Name(n) = tp {
                    if let Some(Some(arg)) = b.get(n.id.as_str()).cloned() {
                        return self.type_repr(&arg.expr, &arg.env, &arg.bindings);
                    }
                    if b.contains_key(n.id.as_str()) {
                        return Ok(n.id.as_str().to_owned());
                    }
                }
                let name = base_name(tp).unwrap_or_default().to_owned();
                match self.eval_expr(tp, env) {
                    Ok(v) if self.alias_named(&name, &v).is_some() => name,
                    Ok(Value::Class(c)) => class_qualified(&c),
                    Ok(Value::Native(n)) if is_builtin_type(n.name) => n.name.to_owned(),
                    Ok(Value::Instance(inst)) if inst.class.name == "NewType" => {
                        self.repr_of(&Value::Instance(inst))?
                    }
                    _ => format!("typing.{name}"),
                }
            }
            Expr::Subscript(s) => {
                let name = base_name(&s.value).unwrap_or_default().to_owned();
                let args = args_of(&s.slice);
                let (origin, alias) = self.cast_origin(&s.value, env)?;
                let rendered = match origin {
                    Origin::Literal => args
                        .iter()
                        .map(|a| {
                            let v = self.eval_expr(a, env)?;
                            self.repr_of(&v)
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    _ => args
                        .iter()
                        .map(|a| self.type_repr(a, env, b))
                        .collect::<Result<Vec<_>, _>>()?,
                };
                let inner = rendered.join(", ");
                match origin {
                    Origin::Optional => format!("typing.Optional[{inner}]"),
                    Origin::Union => {
                        let nones = args.iter().filter(|a| is_none_expr(a)).count();
                        if args.len() == 2 && nones == 1 {
                            let other = rendered
                                .iter()
                                .zip(&args)
                                .find(|(_, a)| !is_none_expr(a))
                                .map(|(r, _)| r.clone())
                                .unwrap_or_default();
                            format!("typing.Optional[{other}]")
                        } else {
                            let parts: Vec<String> = rendered
                                .iter()
                                .zip(&args)
                                .map(|(r, a)| {
                                    if is_none_expr(a) {
                                        "NoneType".to_owned()
                                    } else {
                                        r.clone()
                                    }
                                })
                                .collect();
                            format!("typing.Union[{}]", parts.join(", "))
                        }
                    }
                    Origin::Literal | Origin::Annotated => format!("typing.{name}[{inner}]"),
                    Origin::Kind(_) => format!("{}{name}[{inner}]", typing_prefix(&name)),
                    Origin::Refused => match (alias, self.eval_expr(&s.value, env).ok()) {
                        (Some(_), _) => format!("{name}[{inner}]"),
                        (None, Some(Value::Class(c))) => {
                            format!("{}[{inner}]", class_qualified(&c))
                        }
                        (None, Some(Value::Native(n))) if is_builtin_type(n.name) => {
                            format!("{}[{inner}]", n.name)
                        }
                        _ => format!("typing.{name}[{inner}]"),
                    },
                }
            }
            other => {
                let v = self.eval_expr(other, env)?;
                self.repr_of(&v)?
            }
        })
    }
}

fn is_none_expr(e: &Expr) -> bool {
    matches!(e, Expr::NoneLiteral(_))
}

/// `module.QualName` for a class, as `typing` prints it (builtins bare).
fn class_qualified(c: &Rc<crate::value::Class>) -> String {
    let module = match c.class_attrs.borrow().get("__module__") {
        Some(Value::Str(m)) => (**m).clone(),
        _ => "__main__".to_owned(),
    };
    if module == "builtins" {
        c.name.clone()
    } else {
        format!("{module}.{}", c.name)
    }
}

/// Builtin types the VM represents as natives.
fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "int"
            | "float"
            | "complex"
            | "str"
            | "bool"
            | "bytes"
            | "bytearray"
            | "list"
            | "dict"
            | "set"
            | "frozenset"
            | "tuple"
            | "object"
            | "type"
            | "range"
            | "slice"
            | "memoryview"
    )
}

/// `type(a) is type(b)` for `Literal` membership: `True` is not `1`.
fn same_exact_type(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Instance(x), Value::Instance(y)) => Rc::ptr_eq(&x.class, &y.class),
        (Value::Set(x), Value::Set(y)) => x.frozen.get() == y.frozen.get(),
        (Value::Dict(x), Value::Dict(y)) => x.frozen.get() == y.frozen.get(),
        _ => std::mem::discriminant(a) == std::mem::discriminant(b),
    }
}
