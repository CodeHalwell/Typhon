//! Nominal type checker for Typhon (Phase 1).
//!
//! Implements:
//!
//! - A simple [`Type`] enum covering primitives, classes, functions,
//!   unions, generic containers, and `Unknown`.
//! - Translation of Python annotation expressions into [`Type`] values.
//! - Inference of the type of simple expressions (literals, names, calls,
//!   binary operations).
//! - Assignment compatibility: the value on the right of an annotated
//!   assignment must be a subtype of the annotation.
//! - Non-nullable types: an annotation of `T` rejects `None` and
//!   `T | None` values; users must guard with `if x is not None:` or
//!   `isinstance(x, T)` to narrow.
//! - Flow narrowing inside `if x is None:`, `if x is not None:`, and
//!   `if isinstance(x, T):` branches.
//!
//! This is intentionally lightweight: structural subtyping, generics with
//! inference, and protocols are deferred to Phase 3. The aim of Phase 1 is
//! useful diagnostics on a meaningful subset of programs, not full
//! coverage.

use std::collections::{HashMap, HashSet};

use ruff_python_ast::{Expr, MatchCase, ModModule, Number, Operator, Pattern, Stmt};
use ruff_text_size::{Ranged, TextRange};
use tyc_diagnostics::{Diagnostics, TycError};
use tyc_resolve::{Binding, BindingKind, ResolvedModule, ScopeId};

/// A simplified type representation suitable for Phase 1.
///
/// Generics are *named* but not parameterised structurally — we record
/// `list[int]` as `Generic("list", [Int])`, and treat `list[X]` and
/// `list[Y]` as different types only at the assignment-check level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    /// `int`
    Int,
    /// `str`
    Str,
    /// `bool`
    Bool,
    /// `float`
    Float,
    /// `bytes`
    Bytes,
    /// `None` (the singleton type, not the type *containing* None).
    None,
    /// A user-defined class.
    Class(String),
    /// A function with parameter types and a return type.
    /// When `variadic` is true, the function accepts any number of arguments
    /// beyond its declared `params` (used for built-ins like `env`).
    Function {
        params: Vec<Type>,
        ret: Box<Type>,
        variadic: bool,
    },
    /// `Container[Args...]` — e.g. `list[int]`, `dict[str, int]`.
    Generic(String, Vec<Type>),
    /// A union of types. Always at least two variants; canonicalised on
    /// construction so that variants are sorted-by-display and duplicates
    /// are dropped.
    Union(Vec<Type>),
    /// `Any` — top type. Compatible with anything in either direction.
    Any,
    /// Unresolved type (annotation we don't yet understand).
    Unknown,
    /// A PEP 695 type parameter that has not been bound yet.  Behaves like
    /// `Any` in assignability checks (permissive in both directions) until
    /// the call-site inference pass substitutes it with a concrete type.
    /// The string is the type-parameter name as declared in the source.
    TypeVar(String),
}

impl Type {
    /// Build a union over `types`, simplifying:
    /// - removes duplicates,
    /// - flattens nested unions,
    /// - reduces single-element unions to their element.
    pub fn union_of(types: Vec<Type>) -> Type {
        let mut flat = Vec::new();
        for t in types {
            match t {
                Type::Union(xs) => flat.extend(xs),
                other => flat.push(other),
            }
        }
        flat.dedup_by(|a, b| a == b);
        // Manual dedup that doesn't require sorting.
        let mut unique: Vec<Type> = Vec::new();
        for t in flat {
            if !unique.contains(&t) {
                unique.push(t);
            }
        }
        match unique.len() {
            0 => Type::Unknown,
            1 => unique.into_iter().next().unwrap(),
            _ => Type::Union(unique),
        }
    }

    /// Construct `T | None`.
    pub fn optional(inner: Type) -> Type {
        Self::union_of(vec![inner, Type::None])
    }

    /// True if `None` is part of this type.
    pub fn is_nullable(&self) -> bool {
        match self {
            Type::None => true,
            Type::Union(xs) => xs.iter().any(|t| matches!(t, Type::None)),
            _ => false,
        }
    }

    /// Return `self` with `None` removed from any union.
    pub fn strip_none(&self) -> Type {
        match self {
            Type::None => Type::Unknown,
            Type::Union(xs) => {
                let kept: Vec<Type> = xs
                    .iter()
                    .filter(|t| !matches!(t, Type::None))
                    .cloned()
                    .collect();
                Type::union_of(kept)
            }
            other => other.clone(),
        }
    }

    /// Display name for diagnostics.
    pub fn display(&self) -> String {
        match self {
            Type::Int => "int".into(),
            Type::Str => "str".into(),
            Type::Bool => "bool".into(),
            Type::Float => "float".into(),
            Type::Bytes => "bytes".into(),
            Type::None => "None".into(),
            Type::Class(n) => n.clone(),
            Type::Function { params, ret, .. } => {
                let p: Vec<String> = params.iter().map(|t| t.display()).collect();
                format!("({}) -> {}", p.join(", "), ret.display())
            }
            Type::Generic(name, args) => {
                let a: Vec<String> = args.iter().map(|t| t.display()).collect();
                format!("{}[{}]", name, a.join(", "))
            }
            Type::Union(xs) => {
                let s: Vec<String> = xs.iter().map(|t| t.display()).collect();
                s.join(" | ")
            }
            Type::Any => "Any".into(),
            Type::TypeVar(name) => name.clone(),
            Type::Unknown => "?".into(),
        }
    }
}

/// Check whether a value of type `actual` is assignable to a target of
/// type `expected`.
///
/// Phase-1 rules (loose but useful):
///
/// - `expected = Any` or `actual = Any` ⇒ allowed.
/// - `expected = Unknown` or `actual = Unknown` ⇒ allowed (skip).
/// - `expected = Float`, `actual = Int` ⇒ allowed (numeric widening).
/// - `expected = Union`, `actual` ⇒ allowed if `actual` is assignable
///   to any variant.
/// - `actual = Union` ⇒ allowed only if every variant is assignable
///   to `expected`.
/// - Generic types match on the head name and check each arg pairwise,
///   consulting [`generic_param_variance`] per parameter so
///   `list[T]` (mutable container) stays invariant while
///   `Sequence[T]` / `Iterable[T]` (read-only view) flow covariantly.
/// - Otherwise structural equality.
pub fn assignable(expected: &Type, actual: &Type) -> bool {
    match (expected, actual) {
        (Type::Any, _) | (_, Type::Any) => true,
        // `object` is Python's universal base type — every value is an
        // instance, so anything is assignable to a `Class("object")`
        // expectation. This makes `list[dict[str, object]]` accept
        // `[{"name": "x"}]` without invariance fighting nested literals.
        (Type::Class(name), _) if name == "object" => true,
        // Unbound PEP 695 type parameters behave like `Any` until call-site
        // inference (`bind_typevars_and_substitute`) refines them.
        (Type::TypeVar(_), _) | (_, Type::TypeVar(_)) => true,
        (Type::Unknown, _) | (_, Type::Unknown) => true,
        (Type::Float, Type::Int) => true,
        // Union/Union must come before the single-Union arms: every actual
        // variant has to be assignable to *some* expected variant. Falling
        // through to `(Union, other)` then `(other, Union)` recursively
        // requires every actual variant to match every expected variant,
        // which fails for `int | None = int | None`.
        (Type::Union(expected_vs), Type::Union(actual_vs)) => actual_vs
            .iter()
            .all(|a| expected_vs.iter().any(|e| assignable(e, a))),
        (Type::Union(variants), other) => variants.iter().any(|v| assignable(v, other)),
        (other, Type::Union(variants)) => variants.iter().all(|v| assignable(other, v)),
        (Type::Generic(an, aa), Type::Generic(bn, bb)) => {
            // Result[T, E] accepts Ok[T] (success) or Err[E] (failure) as values.
            // Only return when a constructor matches; otherwise fall through to
            // the structural generic check so Result[T,E] = Result[T,E] works.
            if an == "Result" && aa.len() == 2 {
                match (bn.as_str(), bb.len()) {
                    ("Ok", 1) => return assignable(&aa[0], &bb[0]),
                    ("Err", 1) => return assignable(&aa[1], &bb[0]),
                    _ => {}
                }
            }
            if an != bn || aa.len() != bb.len() {
                return false;
            }
            aa.iter()
                .zip(bb)
                .enumerate()
                .all(|(idx, (formal, actual_arg))| {
                    match generic_param_variance(an, idx) {
                        // Covariant: `Box[Sub]` flows into `Box[Super]`. The
                        // recursive call mirrors the outer assignability rule,
                        // including primitive widening (`Int -> Float`).
                        Variance::Covariant => assignable(formal, actual_arg),
                        // Contravariant (callable args): `Callable[[Animal]]`
                        // accepts a callable taking `Cat` because anyone able
                        // to handle the wider type can handle the narrower
                        // one. Direction is flipped: actual must be assignable
                        // *to* formal.
                        Variance::Contravariant => assignable(actual_arg, formal),
                        // Invariant: the parameter must match exactly. This
                        // is the safe default for mutable containers (`list`,
                        // `set`, `dict[K]`) — covariance there is unsound
                        // because a write through the wider view can break
                        // readers of the narrower view. Bidirectional
                        // assignability captures structural equality without
                        // forcing `PartialEq` on every `Type` arm.
                        Variance::Invariant => {
                            assignable(formal, actual_arg) && assignable(actual_arg, formal)
                        }
                    }
                })
        }
        // Bare (unparameterized) `Ok` or `Err` is assignable to `Result[T, E]`
        // without parameter checking. This arises when the `?` operator expands
        // into `if isinstance(x, Err): return x`, and the isinstance-narrowing
        // reduces `x` from `Result[T, E]` to bare `Class("Err")`.
        (Type::Generic(an, _), Type::Class(cn))
            if an == "Result" && (cn == "Ok" || cn == "Err") =>
        {
            true
        }
        // Bare-container annotations (`list`, `dict`, `tuple`, `set`,
        // `frozenset`) act as `name[Any]` — they accept any
        // parameterisation. Without this rule, `let xs: list = []`
        // produces the misleading "expected `list`, found `list[?]`"
        // diagnostic from FINDINGS #33 because the RHS infers as
        // `Generic("list", [Unknown])` but the annotation is
        // `Class("list")`.
        (Type::Class(en), Type::Generic(an, _)) if en == an && is_bare_container_name(en) => true,
        (a, b) => a == b,
    }
}

/// Return `true` when the type is an unbound PEP 695 type parameter.
/// Used by the container-literal widening rules to avoid prematurely
/// resolving a TypeVar to a concrete element type (which would block
/// PEP 695 inference from binding it from the actual arguments).
fn is_typevar(t: &Type) -> bool {
    matches!(t, Type::TypeVar(_))
}

/// Return `true` if `name` is a built-in container type whose bare
/// (unparameterised) form should be treated as accepting any
/// parameterisation (`list` ≡ `list[Any]`, etc.).
fn is_bare_container_name(name: &str) -> bool {
    matches!(
        name,
        "list" | "dict" | "tuple" | "set" | "frozenset" | "deque"
    )
}

/// Variance of a type parameter — controls the assignability direction
/// applied when comparing two `Generic` types with matching heads.
///
/// PEP 484 / 695 treat all parameters as invariant by default. Typhon
/// mirrors that for unknown / user-defined generics so a user class
/// `class Box[T]` is sound regardless of how `T` is used internally;
/// individual stdlib generics declare their own variance below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variance {
    /// `T` only flows out of the container (read-only). `Seq[Sub]`
    /// accepts `Seq[Super]` when reading.
    Covariant,
    /// `T` only flows in (write-only). `Sink[Super]` accepts `Sink[Sub]`.
    Contravariant,
    /// `T` flows both ways. Must match exactly. Default for mutable
    /// containers (`list`, `set`, `dict` value position) and any
    /// user-defined generic.
    Invariant,
}

/// Variance of the `idx`-th type parameter of the generic head `head`.
///
/// Hand-curated map covering the Python built-ins and typing-module
/// abstract base classes Typhon's stdlib stubs reference. Anything not
/// listed defaults to [`Variance::Invariant`], which is the safe choice
/// — a misclassified user generic that should be covariant rejects
/// otherwise-valid programs, but a misclassified mutable container that
/// flows covariantly silently accepts unsound programs. The former is
/// fixable by writing the obvious cast; the latter is a real bug source.
pub fn generic_param_variance(head: &str, idx: usize) -> Variance {
    match (head, idx) {
        // ── Mutable containers — invariant in every position. ─────────
        ("list", 0)
        | ("set", 0)
        | ("MutableSequence", 0)
        | ("MutableSet", 0)
        | ("MutableMapping", 0)
        | ("MutableMapping", 1)
        | ("Dict", 0)
        | ("Dict", 1)
        | ("dict", 0)
        | ("dict", 1)
        | ("Set", 0)
        | ("List", 0) => Variance::Invariant,

        // ── Read-only views / immutable containers — covariant in T. ──
        ("Sequence", 0)
        | ("Iterable", 0)
        | ("Iterator", 0)
        | ("Container", 0)
        | ("Collection", 0)
        | ("Reversible", 0)
        | ("AbstractSet", 0)
        | ("FrozenSet", 0)
        | ("frozenset", 0)
        | ("tuple", 0)
        | ("Tuple", 0)
        | ("Awaitable", 0)
        | ("Coroutine", 0)
        | ("AsyncIterable", 0)
        | ("AsyncIterator", 0)
        | ("Generator", 0)
        | ("AsyncGenerator", 0)
        | ("ContextManager", 0) => Variance::Covariant,

        // ── Mapping[K, V] — invariant in K (keys are hashed/compared
        // exactly) and covariant in V (values flow out via __getitem__).
        ("Mapping", 0) => Variance::Invariant,
        ("Mapping", 1) => Variance::Covariant,

        // ── Optional[T] / Union flattening — covariant. ───────────────
        ("Optional", 0) => Variance::Covariant,

        // ── Callable[[Args], Ret] — args contravariant, return
        // covariant. The args position is encoded as parameter 0 in
        // Typhon's `Generic` shape; the return is parameter 1. (A more
        // structured Callable type can refine this later — today's
        // single-arg shape already gets us the right direction.) ──────
        ("Callable", 0) => Variance::Contravariant,
        ("Callable", 1) => Variance::Covariant,

        // ── Result[T, E] — Ok and Err payloads only flow out, so both
        // positions are covariant (a Result[Sub, Err] is a Result[Super, Err]). ─
        ("Result", 0) | ("Result", 1) => Variance::Covariant,

        // Unknown head / parameter — default invariant.
        _ => Variance::Invariant,
    }
}

/// Translate an annotation expression into a [`Type`].
///
/// `classes` is the set of class names declared in the enclosing module so
/// we can resolve nominal references.
pub fn type_from_annotation(expr: &Expr, classes: &[String]) -> Type {
    type_from_annotation_with_params(expr, classes, &[])
}

/// Same as [`type_from_annotation`] but treats every name in `type_params`
/// as `Type::Any` so that PEP 695 generic functions don't trip the
/// assignability check before we have a real inference engine.
pub fn type_from_annotation_with_params(
    expr: &Expr,
    classes: &[String],
    type_params: &[String],
) -> Type {
    match expr {
        Expr::Name(n) => match n.id.as_str() {
            "int" => Type::Int,
            "str" => Type::Str,
            "bool" => Type::Bool,
            "float" => Type::Float,
            "bytes" => Type::Bytes,
            "None" => Type::None,
            "Any" => Type::Any,
            // A type parameter (PEP 695) — preserved as `Type::TypeVar` so
            // call-site inference can substitute it with the concrete
            // argument type.  Assignability still treats it as `Any` until
            // substitution happens, so existing tests that compose generic
            // signatures continue to type-check.
            other if type_params.iter().any(|p| p == other) => Type::TypeVar(other.to_owned()),
            other if classes.iter().any(|c| c == other) => Type::Class(other.to_owned()),
            // Unknown but treat as nominal class (may be imported).
            other => Type::Class(other.to_owned()),
        },
        Expr::BinOp(b) if matches!(b.op, Operator::BitOr) => {
            let left = type_from_annotation_with_params(&b.left, classes, type_params);
            let right = type_from_annotation_with_params(&b.right, classes, type_params);
            Type::union_of(vec![left, right])
        }
        Expr::Subscript(s) => {
            // Accept either a bare name (`Optional[T]`) or an attribute
            // (`typing.Optional[T]`, `typing.Union[A, B]`).
            let head = match s.value.as_ref() {
                Expr::Name(n) => n.id.as_str().to_owned(),
                Expr::Attribute(a) => a.attr.as_str().to_owned(),
                _ => return Type::Unknown,
            };
            // Optional[T] → T | None
            if head == "Optional" {
                return Type::optional(type_from_annotation_with_params(
                    &s.slice,
                    classes,
                    type_params,
                ));
            }
            // Union[A, B, ...] / typing.Union[...]
            if head == "Union" {
                if let Expr::Tuple(t) = s.slice.as_ref() {
                    let args: Vec<Type> = t
                        .elts
                        .iter()
                        .map(|e| type_from_annotation_with_params(e, classes, type_params))
                        .collect();
                    return Type::union_of(args);
                }
                return type_from_annotation_with_params(&s.slice, classes, type_params);
            }
            // Callable[[P1, P2, ...], R] / Callable[..., R] — structural
            // function type. Map to `Type::Function` so call expressions
            // can be type-checked against the param list and the value is
            // accepted by the call-site arm rather than rejected as
            // `not_callable`. FINDINGS #43.
            if head == "Callable" {
                if let Expr::Tuple(t) = s.slice.as_ref() {
                    if t.elts.len() == 2 {
                        let ret =
                            type_from_annotation_with_params(&t.elts[1], classes, type_params);
                        match &t.elts[0] {
                            // `Callable[[T, U], R]`
                            Expr::List(list) => {
                                let params: Vec<Type> = list
                                    .elts
                                    .iter()
                                    .map(|e| {
                                        type_from_annotation_with_params(e, classes, type_params)
                                    })
                                    .collect();
                                return Type::Function {
                                    params,
                                    ret: Box::new(ret),
                                    variadic: false,
                                };
                            }
                            // `Callable[..., R]` — any args, fixed return.
                            // Mirror Python's behaviour by treating the
                            // arity as "any", which we model with a
                            // single-param variadic function.
                            Expr::EllipsisLiteral(_) => {
                                return Type::Function {
                                    params: vec![Type::Any],
                                    ret: Box::new(ret),
                                    variadic: true,
                                };
                            }
                            _ => {}
                        }
                    }
                }
                // Unrecognised shape — leave as a generic so existing
                // variance / assignability checks keep working.
                return Type::Generic(
                    "Callable".into(),
                    vec![Type::Unknown, Type::Unknown],
                );
            }
            // Result[T, E] — two-parameter sealed sum type (Ok[T] | Err[E]).
            if head == "Result" {
                if let Expr::Tuple(t) = s.slice.as_ref() {
                    if t.elts.len() == 2 {
                        let ok_type =
                            type_from_annotation_with_params(&t.elts[0], classes, type_params);
                        let err_type =
                            type_from_annotation_with_params(&t.elts[1], classes, type_params);
                        return Type::Generic("Result".into(), vec![ok_type, err_type]);
                    }
                }
                return Type::Generic("Result".into(), vec![Type::Unknown, Type::Unknown]);
            }
            // list[int], dict[str, int], tuple[int, str, ...]
            let args: Vec<Type> = match s.slice.as_ref() {
                Expr::Tuple(t) => t
                    .elts
                    .iter()
                    .map(|e| type_from_annotation_with_params(e, classes, type_params))
                    .collect(),
                other => vec![type_from_annotation_with_params(
                    other,
                    classes,
                    type_params,
                )],
            };
            Type::Generic(head, args)
        }
        Expr::NoneLiteral(_) => Type::None,
        _ => Type::Unknown,
    }
}

/// Walk `ty` and collect every `(name, position)` pair where a typevar
/// appears.  The resulting structure feeds [`bind_typevars`] which walks
/// the same positions of an actual argument type to read off bindings.
fn collect_typevar_positions(ty: &Type) -> Vec<String> {
    let mut out = Vec::new();
    walk_typevars(ty, &mut out);
    out
}

fn walk_typevars(ty: &Type, out: &mut Vec<String>) {
    match ty {
        Type::TypeVar(name) => {
            if !out.contains(name) {
                out.push(name.clone());
            }
        }
        Type::Union(xs) | Type::Generic(_, xs) => {
            for x in xs {
                walk_typevars(x, out);
            }
        }
        Type::Function { params, ret, .. } => {
            for p in params {
                walk_typevars(p, out);
            }
            walk_typevars(ret, out);
        }
        _ => {}
    }
}

/// Try to bind a `Type::TypeVar` to a concrete actual type by walking
/// `formal` and `actual` in lockstep.  Recursive: `list[T]` against
/// `list[int]` binds `T → int`.  Any binding conflict resolves by union
/// (so calling `f[T](a: T, b: T)` with `(int, str)` infers `T = int|str`).
///
/// Handles unions on either side so that:
/// - `Optional[T]` (i.e. `T | None`) against `int` binds `T → int` by
///   pairing the TypeVar variant with the non-None actual type.
/// - `list[T]` against `list[int] | list[str]` binds `T → int | str`.
fn bind_typevars(
    formal: &Type,
    actual: &Type,
    bindings: &mut std::collections::HashMap<String, Type>,
) {
    match (formal, actual) {
        (Type::TypeVar(name), other) => {
            // Suppress self-bindings (`T → T`): they're uninformative and,
            // worse, mark the TypeVar as "bound" which then prevents the
            // backward bidirectional pass from pinning it from the expected
            // type.  Self-bindings arise when an empty literal's element
            // type is propagated from a generic formal (e.g. `head([])`
            // with `xs: list[T]` infers the literal as `list[T]`, then the
            // forward pass would otherwise insert `T → T`).
            if let Type::TypeVar(other_name) = other {
                if other_name == name {
                    return;
                }
            }
            if let Some(existing) = bindings.get(name).cloned() {
                if existing != *other {
                    bindings.insert(name.clone(), Type::union_of(vec![existing, other.clone()]));
                }
            } else {
                bindings.insert(name.clone(), other.clone());
            }
        }
        (Type::Generic(fh, fa), Type::Generic(ah, aa)) if fh == ah && fa.len() == aa.len() => {
            for (f, a) in fa.iter().zip(aa) {
                bind_typevars(f, a, bindings);
            }
        }
        (
            Type::Function {
                params: fp,
                ret: fr,
                ..
            },
            Type::Function {
                params: ap,
                ret: ar,
                ..
            },
        ) if fp.len() == ap.len() => {
            for (f, a) in fp.iter().zip(ap) {
                bind_typevars(f, a, bindings);
            }
            bind_typevars(fr, ar, bindings);
        }
        // `Optional[T]` (formal `T | None`) against a concrete actual:
        // narrow each formal variant against the non-None portion of
        // `actual`, so `T` doesn't get widened to `int | None`.
        (Type::Union(fv), other) => {
            let actual_stripped = other.strip_none();
            for variant in fv {
                if matches!(variant, Type::None) {
                    continue;
                }
                bind_typevars(variant, &actual_stripped, bindings);
            }
        }
        // Formal contains a TypeVar but actual is a Union — bind each
        // variant of the actual against the formal so we accumulate the
        // full set (e.g. `list[T]` against `list[int] | list[str]`).
        (f, Type::Union(av)) if !collect_typevar_positions(f).is_empty() => {
            for variant in av {
                bind_typevars(f, variant, bindings);
            }
        }
        _ => {}
    }
}

/// Substitute every `TypeVar` in `ty` whose name appears in `bindings`
/// with the bound type, recursively.  Unbound type vars are left as
/// `TypeVar` so the caller can still see them in `Display` output.
fn substitute_typevars(ty: &Type, bindings: &std::collections::HashMap<String, Type>) -> Type {
    match ty {
        Type::TypeVar(name) => bindings.get(name).cloned().unwrap_or_else(|| ty.clone()),
        Type::Union(xs) => Type::union_of(
            xs.iter()
                .map(|x| substitute_typevars(x, bindings))
                .collect(),
        ),
        Type::Generic(h, args) => Type::Generic(
            h.clone(),
            args.iter()
                .map(|x| substitute_typevars(x, bindings))
                .collect(),
        ),
        Type::Function {
            params,
            ret,
            variadic,
        } => Type::Function {
            params: params
                .iter()
                .map(|p| substitute_typevars(p, bindings))
                .collect(),
            ret: Box::new(substitute_typevars(ret, bindings)),
            variadic: *variadic,
        },
        other => other.clone(),
    }
}

/// Top-level entry point for call-site PEP 695 inference.  Given the
/// formal parameter types and actual argument types, infer the binding
/// for every TypeVar mentioned in the formals, then substitute those
/// bindings in `return_type`.  The result is the call's inferred return.
pub fn bind_typevars_and_substitute(
    formal_params: &[Type],
    actual_args: &[Type],
    return_type: &Type,
) -> Type {
    bind_typevars_and_substitute_bidirectional(formal_params, actual_args, return_type, None)
}

/// Same as [`bind_typevars_and_substitute`] but also lets the call's
/// *expected* return type drive inference for TypeVars that the
/// arguments alone leave unbound — true bidirectional inference for
/// PEP 695 generic calls.
///
/// Two-phase algorithm:
///
/// 1. **Forward pass.** Walk every (formal, actual) pair and read off
///    bindings as before.  This is enough for most calls.
/// 2. **Backward pass.** If `expected_return` is `Some(_)` and the
///    declared return type still contains an unbound TypeVar after
///    phase 1, walk `return_type` against `expected_return` so the
///    annotation at the call site can pin the TypeVar.
///
/// Example: `def make[T]() -> list[T]: ...` then
/// `let xs: list[int] = make()`.  Phase 1 has no args to consult, so
/// `T` stays unbound; phase 2 sees `list[T]` vs `list[int]` and binds
/// `T → int`, giving the call a concrete `list[int]` return type.
pub fn bind_typevars_and_substitute_bidirectional(
    formal_params: &[Type],
    actual_args: &[Type],
    return_type: &Type,
    expected_return: Option<&Type>,
) -> Type {
    let bindings =
        compute_bidirectional_bindings(formal_params, actual_args, return_type, expected_return);
    substitute_typevars(return_type, &bindings)
}

/// Compute the full set of TypeVar bindings produced by a bidirectional
/// inference pass at a call site.  Splits the two-phase work out so the
/// bound-check at the call site can validate against the same final
/// bindings the substitution will use — otherwise TypeVars pinned only
/// by the backward (expected-return) pass would bypass their declared
/// bounds.
pub fn compute_bidirectional_bindings(
    formal_params: &[Type],
    actual_args: &[Type],
    return_type: &Type,
    expected_return: Option<&Type>,
) -> std::collections::HashMap<String, Type> {
    let mut bindings: std::collections::HashMap<String, Type> = std::collections::HashMap::new();
    // Skip work when no formal mentions a TypeVar.
    let has_typevar = formal_params
        .iter()
        .chain(std::iter::once(return_type))
        .any(|t| {
            let mut tmp = Vec::new();
            walk_typevars(t, &mut tmp);
            !tmp.is_empty()
        });
    if !has_typevar {
        return bindings;
    }
    for (formal, actual) in formal_params.iter().zip(actual_args.iter()) {
        bind_typevars(formal, actual, &mut bindings);
    }
    // Backward pass: pin any TypeVars in the return type that the
    // arguments left unbound, using the call-site expected type.
    // Bindings established by the forward pass are authoritative — args
    // carry stronger evidence than annotations — so we collect the
    // backward result into a fresh map and only use it for TypeVars the
    // forward pass didn't touch.  Without this, calling `f[T](x: T) -> T`
    // with `int` under a `str` annotation would widen `T` to `int | str`
    // and silently lose the assignment-site mismatch.
    if let Some(expected) = expected_return {
        let mut return_tvs = Vec::new();
        walk_typevars(return_type, &mut return_tvs);
        let any_unbound = return_tvs.iter().any(|n| !bindings.contains_key(n));
        if any_unbound {
            let mut backward: std::collections::HashMap<String, Type> =
                std::collections::HashMap::new();
            bind_typevars(return_type, expected, &mut backward);
            for (name, ty) in backward {
                bindings.entry(name).or_insert(ty);
            }
        }
    }
    bindings
}

/// Used by tests in lower layers (resolver, db) that want to enumerate
/// known typevar names in a type expression without depending on the
/// internal walker directly.
#[doc(hidden)]
pub fn typevars_in(ty: &Type) -> Vec<String> {
    collect_typevar_positions(ty)
}

/// Collect the names of PEP 695 type parameters into a flat list.
pub fn collect_type_param_names(type_params: &[ruff_python_ast::TypeParam]) -> Vec<String> {
    type_params
        .iter()
        .map(|tp| match tp {
            ruff_python_ast::TypeParam::TypeVar(t) => t.name.as_str().to_owned(),
            ruff_python_ast::TypeParam::ParamSpec(p) => p.name.as_str().to_owned(),
            ruff_python_ast::TypeParam::TypeVarTuple(t) => t.name.as_str().to_owned(),
        })
        .collect()
}

/// One entry in the per-scope type environment.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `span` is filled in for use by future narrowing diagnostics.
struct TypeBinding {
    name: String,
    /// The static (declared) type of the binding.
    declared: Type,
    /// The current narrowed type — equal to `declared` unless an enclosing
    /// guard has narrowed it.
    narrowed: Type,
    /// Span of the original declaration (for diagnostics).
    span: (usize, usize),
}

/// Type-environment stack — a map of name → TypeBinding per scope.
#[derive(Debug, Default, Clone)]
struct TypeEnv {
    /// `scopes[i].get(name)` → binding.
    scopes: Vec<HashMap<String, TypeBinding>>,
}

impl TypeEnv {
    fn enter(&mut self) {
        self.scopes.push(HashMap::new());
    }
    fn leave(&mut self) {
        self.scopes.pop();
    }
    fn declare(&mut self, b: TypeBinding) {
        if let Some(top) = self.scopes.last_mut() {
            top.insert(b.name.clone(), b);
        }
    }
    fn lookup(&self, name: &str) -> Option<&TypeBinding> {
        for scope in self.scopes.iter().rev() {
            if let Some(b) = scope.get(name) {
                return Some(b);
            }
        }
        None
    }
    /// Apply a narrowing within the topmost frame for the given name.
    fn narrow(&mut self, name: &str, new_type: Type) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(b) = scope.get_mut(name) {
                b.narrowed = new_type;
                return;
            }
        }
    }
    fn snapshot(&self) -> Vec<HashMap<String, TypeBinding>> {
        self.scopes.clone()
    }
    fn restore(&mut self, snap: Vec<HashMap<String, TypeBinding>>) {
        self.scopes = snap;
    }
}

/// Per-module check state.
struct Checker<'a> {
    path: String,
    source: &'a str,
    resolved: &'a ResolvedModule,
    classes: Vec<String>,
    /// For each declared function name, its inferred signature type.
    function_signatures: HashMap<String, Type>,
    /// Per-function arity metadata that doesn't fit in `Type::Function`
    /// (which only tracks positional types + a single `variadic` flag).
    /// Indexed by function name; entries are populated alongside
    /// `function_signatures` from the AST so the call-site check can
    /// honour default values, `*args`, `**kwargs`, and keyword arguments
    /// without rejecting valid calls (FINDINGS #44).
    function_arity_info: HashMap<String, ArityInfo>,
    /// Bounds declared on PEP 695 type parameters, keyed by function name.
    /// E.g. `def f[T: Interface](x: T)` populates `{"f": {"T": Class("Interface")}}`.
    /// Checked at call sites via `Checker::check_call_typevar_bounds`.
    function_type_bounds: HashMap<String, HashMap<String, Type>>,
    /// The bounds in effect for the function body currently being checked.
    /// Populated by `check_function` from `function_type_bounds` so that
    /// `Expr::Attribute` resolution can look up what interface a `T: Iface`
    /// parameter conforms to.
    active_typevar_bounds: HashMap<String, Type>,
    /// Sealed union declarations: name → ordered list of variant class names.
    /// Populated from `type Foo = A | B | C` statements in the first pass.
    sealed_unions: HashMap<String, Vec<String>>,
    /// Interfaces (Typhon `interface Name:` → `class Name(Protocol):`).
    /// Maps the interface name to its required member shape and whether it
    /// opted in to runtime checking via `@runtime_checkable`. In v1 we
    /// check member presence only; full signature compatibility is deferred.
    interfaces: HashMap<String, InterfaceDecl>,
    /// All classes declared in the module along with their declared member
    /// names.  Used for structural conformance against an interface.
    class_shapes: HashMap<String, InterfaceShape>,
    /// Classes declared with the `frozen` modifier (`class Foo frozen:`).
    /// Used to reject attribute writes to instances of these classes at
    /// check time — matches the runtime behaviour of the emitted
    /// `@dataclass(frozen=True)` decorator (`FrozenInstanceError`).
    frozen_classes: std::collections::HashSet<String>,
    /// Class inheritance: maps each class name to its direct base class names
    /// as written in the source (`class Dog(Animal):` → `{"Dog": ["Animal"]}`).
    /// Used by `class_inherits_from` to resolve nominal subtype relationships.
    class_parents: HashMap<String, Vec<String>>,
    env: TypeEnv,
    diagnostics: Diagnostics,
    /// Return type of the function whose body we are currently checking
    /// (None at module scope).
    current_return: Option<Type>,
    /// Name of the class whose body we are currently checking, including
    /// the `__typhon_impl_<NAME>` pseudo-class form. Used to give an
    /// unannotated `self` parameter the enclosing class's type so writes
    /// to `self.field` participate in the frozen-class check.
    current_class: Option<String>,
    /// Bumped on entry to an `unsafe:` block, decremented on exit.  While
    /// positive, diagnostics produced by [`Checker::push_error`] /
    /// [`Checker::push_warning`] are dropped so the user can interface with
    /// untyped Python without fighting the checker.  Boundary checks at
    /// assignment sites outside the block still apply normally.
    unsafe_depth: u32,
    /// Byte offsets of the `if True:` statements that correspond to
    /// `unsafe:` blocks.  Computed from the preprocessor's `unsafe_lines`
    /// metadata; queried by [`check_stmt`] when entering an `if` body to
    /// decide whether to bump `unsafe_depth`.
    unsafe_line_starts: Vec<u32>,
}

/// Per-function arity metadata kept alongside `Type::Function` so the
/// call-site arity check can honour default values, `*args`, `**kwargs`,
/// and keyword-argument matching (FINDINGS #44).
///
/// `Type::Function`'s `params: Vec<Type>` only tracks positional types,
/// and its single `variadic` flag conflates "has `*args`" with "accepts
/// any extra args". We need richer information to distinguish:
/// - `def f(a, b=10)` → 1 required, 2 optional
/// - `def variadic(*args)` → 0 required, no fixed max, accepts kwargs only via `**kw`
/// - `f(name="x")` keyword-arg matching against `param_names`
#[derive(Debug, Clone, Default)]
struct ArityInfo {
    /// Names of the positional / pos-or-kw / kw-only parameters declared
    /// on the function, in source order. Used to match keyword arguments
    /// at call sites (`f(name="x")`).
    param_names: Vec<String>,
    /// Minimum number of positional arguments the caller must supply
    /// (i.e. count of params without default values, excluding kw-only).
    /// `def f(a, b=10) -> ...` → `min_positional = 1`.
    min_positional: usize,
    /// Maximum number of positional arguments — the total count of
    /// posonlyargs + args. Kw-only params don't count. `None` for
    /// `*args` functions, which accept unbounded positionals.
    max_positional: Option<usize>,
    /// Names of kw-only parameters (after `*` or `*args`).
    kwonly_names: Vec<String>,
    /// Kw-only names that don't have a default value.
    kwonly_required: Vec<String>,
    /// True when the function declares `**kwargs`, accepting any
    /// otherwise-unmatched keyword argument.
    has_kwarg: bool,
}

/// Declared interface (`interface Name:` → `class Name(Protocol):`). Bundles
/// the required member shape with the `@runtime_checkable` opt-in.
#[derive(Debug, Clone, Default)]
struct InterfaceDecl {
    shape: InterfaceShape,
    /// `true` when the interface is decorated `@runtime_checkable`; in that
    /// case `isinstance(x, Iface)` is allowed (the user has acknowledged it
    /// only validates attribute presence).
    runtime_checkable: bool,
}

/// Parameter count (excluding receiver) and declared return type for an
/// interface or class method.  `return_type = Type::Unknown` means the
/// method is unannotated; unannotated methods satisfy any return-type
/// requirement.
#[derive(Debug, Clone)]
struct MethodSig {
    arity: usize,
    return_type: Type,
}

/// Member shape recorded for an interface or class — methods are recorded as
/// their parameter count (excluding `self`/`cls`), fields as their declared
/// type.
#[derive(Debug, Clone, Default)]
struct InterfaceShape {
    /// Method name → arity + return type.
    methods: HashMap<String, MethodSig>,
    /// Field name → annotation type.
    fields: HashMap<String, Type>,
}

impl InterfaceShape {
    fn member_names(&self) -> Vec<String> {
        let mut all: Vec<String> = self
            .methods
            .keys()
            .chain(self.fields.keys())
            .cloned()
            .collect();
        all.sort();
        all
    }
}

impl<'a> Checker<'a> {
    fn new(path: String, source: &'a str, resolved: &'a ResolvedModule) -> Self {
        Self {
            path,
            source,
            resolved,
            classes: Vec::new(),
            function_signatures: HashMap::new(),
            function_arity_info: HashMap::new(),
            function_type_bounds: HashMap::new(),
            active_typevar_bounds: HashMap::new(),
            interfaces: HashMap::new(),
            class_shapes: HashMap::new(),
            frozen_classes: std::collections::HashSet::new(),
            class_parents: HashMap::new(),
            unsafe_depth: 0,
            unsafe_line_starts: Vec::new(),
            sealed_unions: HashMap::new(),
            env: TypeEnv::default(),
            diagnostics: Diagnostics::new(),
            current_return: None,
            current_class: None,
        }
    }

    /// True iff the `if True:` statement opening at this byte offset
    /// originated from a `unsafe:` block.  Used by `check_if` to gate the
    /// depth counter.
    fn is_unsafe_marker(&self, range: TextRange) -> bool {
        let start = u32::from(range.start());
        self.unsafe_line_starts.binary_search(&start).is_ok()
    }

    /// Assignment compatibility check that accounts for sealed-union subtyping
    /// and structural conformance against `interface` declarations.
    ///
    /// Extends the module-level [`assignable`] function with four rules:
    ///
    /// 1. **Variant → sealed union**: `Circle` is assignable to `Shape` when
    ///    `type Shape = Circle | Rectangle | ...` is declared.
    /// 2. **Class → interface**: a class is assignable to an `interface` when
    ///    its member shape (including inherited members) covers every required
    ///    member of the interface.
    /// 3. **Union expected interception**: when `expected` is a `Union`, retry
    ///    each variant with `is_assignable` so the above rules are available
    ///    inside composite types like `Shape | None` or `list[Shape]`.
    /// 4. **Union actual interception**: when `actual` is a `Union` (e.g. a
    ///    conditional expression narrowed to `Dog | Cat`), every variant must
    ///    be assignable to `expected` so nominal and structural rules apply to
    ///    each arm.
    fn is_assignable(&self, expected: &Type, actual: &Type) -> bool {
        if assignable(expected, actual) {
            return true;
        }
        // Variant → sealed union coercion.
        if let (Type::Class(exp_name), Type::Class(act_name)) = (expected, actual) {
            if let Some(variants) = self.sealed_unions.get(exp_name.as_str()) {
                return variants.iter().any(|v| v == act_name);
            }
            // Structural: actual conforms to expected interface?
            if self.interfaces.contains_key(exp_name.as_str())
                && self.class_conforms_to_interface(act_name, exp_name)
            {
                return true;
            }
            // Nominal: actual inherits from expected class?
            // Interfaces are structural-only: a class that merely lists an
            // interface as a base without implementing its members must not
            // satisfy the interface contract.
            if !self.interfaces.contains_key(exp_name.as_str())
                && self.class_inherits_from(act_name, exp_name)
            {
                return true;
            }
        }
        // For Union expected types (e.g. `Shape | None`), `assignable` recurses
        // using only the base rules. Re-check each variant here so sealed-union
        // knowledge is available in the recursive call.
        if let Type::Union(variants) = expected {
            return variants.iter().any(|v| self.is_assignable(v, actual));
        }
        // For Union actual types (e.g. `Dog | Cat`), every variant must satisfy
        // `expected` so nominal and structural checks apply to each arm.
        if let Type::Union(variants) = actual {
            return variants.iter().all(|v| self.is_assignable(expected, v));
        }
        // Generic / generic (e.g. `Result[T, E] = Ok[V]`, `list[T] = list[V]`):
        // recurse using `is_assignable` for the inner type pairs so sealed
        // unions and interface conformance work *inside* generic containers.
        // The free `assignable` checks above only saw class-name equality on
        // the inner pair; without this arm, `Result[Cmd, str] = Ok(AddCmd(...))`
        // fails when `Cmd` is a sealed union containing `AddCmd`. FINDINGS #45.
        if let (Type::Generic(an, aa), Type::Generic(bn, bb)) = (expected, actual) {
            // Result/Ok / Result/Err variance refinement — mirrors the rule
            // in the free `assignable` but with sealed-union-aware recursion.
            if an == "Result" && aa.len() == 2 {
                match (bn.as_str(), bb.len()) {
                    ("Ok", 1) => return self.is_assignable(&aa[0], &bb[0]),
                    ("Err", 1) => return self.is_assignable(&aa[1], &bb[0]),
                    _ => {}
                }
            }
            if an == bn && aa.len() == bb.len() {
                return aa
                    .iter()
                    .zip(bb)
                    .enumerate()
                    .all(|(idx, (formal, actual_arg))| match generic_param_variance(an, idx) {
                        Variance::Covariant => self.is_assignable(formal, actual_arg),
                        Variance::Contravariant => self.is_assignable(actual_arg, formal),
                        Variance::Invariant => {
                            self.is_assignable(formal, actual_arg)
                                && self.is_assignable(actual_arg, formal)
                        }
                    });
            }
        }
        false
    }

    /// Return `true` if class `cls_name`'s member shape (including inherited
    /// members) covers every required member of `iface_name`'s shape.  Checks
    /// method arity, return type, and annotated field types using
    /// hierarchy-aware `find_method` / `find_field` lookups so that methods
    /// and fields contributed by a base class count toward conformance.
    fn class_conforms_to_interface(&self, cls_name: &str, iface_name: &str) -> bool {
        let Some(iface) = self.interfaces.get(iface_name) else {
            return false;
        };
        // Verify the class exists (it may not be in class_shapes if it was
        // never collected — treat that as non-conforming).
        if !self.class_shapes.contains_key(cls_name) {
            return false;
        }
        let iface_class_type = Type::Class(iface_name.to_owned());
        for (m, iface_sig) in &iface.shape.methods {
            match self.find_method(cls_name, m) {
                Some(cls_sig) if cls_sig.arity == iface_sig.arity => {
                    // Both return types must be known to enforce compatibility.
                    // Unknown return type on either side is treated as compatible
                    // so unannotated methods don't block conformance.
                    // Skip the check when the interface method returns the same
                    // interface type to avoid infinite recursion for
                    // self-referential interfaces (e.g. `def next(self) -> Node`).
                    if iface_sig.return_type != Type::Unknown
                        && cls_sig.return_type != Type::Unknown
                        && iface_sig.return_type != iface_class_type
                        && !self.is_assignable(&iface_sig.return_type, &cls_sig.return_type)
                    {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        for (f, iface_type) in &iface.shape.fields {
            match self.find_field(cls_name, f) {
                Some(cls_type) if self.is_assignable(iface_type, cls_type) => {}
                Some(_) => return false, // field present but wrong type
                None if self.find_method(cls_name, f).is_some_and(|s| s.arity == 0) => {} // property-like method satisfies field
                None => return false,
            }
        }
        true
    }

    /// Return `true` when `child` transitively inherits from `parent` via the
    /// `class_parents` map built during the first collection pass.  Uses an
    /// iterative depth-first search to avoid stack overflow on deep hierarchies.
    fn class_inherits_from(&self, child: &str, parent: &str) -> bool {
        let mut stack: Vec<&str> = vec![child];
        let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
        while let Some(name) = stack.pop() {
            if name == parent {
                return true;
            }
            if !visited.insert(name) {
                continue;
            }
            if let Some(parents) = self.class_parents.get(name) {
                stack.extend(parents.iter().map(String::as_str));
            }
        }
        false
    }

    /// Look up a method by name in `cls_name`'s hierarchy.  Walks `class_parents`
    /// depth-first so methods inherited from a base class are found even when not
    /// directly declared on the queried class.  Returns the first matching
    /// [`MethodSig`] found, or `None` when no class in the hierarchy defines it.
    fn find_method<'b>(&'b self, cls_name: &str, method_name: &str) -> Option<&'b MethodSig> {
        let mut stack: Vec<&str> = vec![cls_name];
        let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
        while let Some(name) = stack.pop() {
            if !visited.insert(name) {
                continue;
            }
            if let Some(shape) = self.class_shapes.get(name) {
                if let Some(sig) = shape.methods.get(method_name) {
                    return Some(sig);
                }
            }
            if let Some(parents) = self.class_parents.get(name) {
                stack.extend(parents.iter().map(String::as_str));
            }
        }
        None
    }

    /// Look up a field by name in `cls_name`'s hierarchy.  Walks `class_parents`
    /// depth-first so fields inherited from a base class are found even when not
    /// directly declared on the queried class.  Returns the first matching
    /// field type found, or `None` when no class in the hierarchy defines it.
    fn find_field<'b>(&'b self, cls_name: &str, field_name: &str) -> Option<&'b Type> {
        let mut stack: Vec<&str> = vec![cls_name];
        let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
        while let Some(name) = stack.pop() {
            if !visited.insert(name) {
                continue;
            }
            if let Some(shape) = self.class_shapes.get(name) {
                if let Some(ty) = shape.fields.get(field_name) {
                    return Some(ty);
                }
            }
            if let Some(parents) = self.class_parents.get(name) {
                stack.extend(parents.iter().map(String::as_str));
            }
        }
        None
    }

    /// Return the missing-member text for a failed interface conformance check.
    /// Returns `None` when the class actually conforms (caller should use
    /// `class_conforms_to_interface` to gate this call).
    fn interface_missing_members(&self, cls_name: &str, iface_name: &str) -> String {
        let iface = match self.interfaces.get(iface_name) {
            Some(i) => i,
            None => return String::new(),
        };
        let iface_class_type = Type::Class(iface_name.to_owned());
        let mut missing = Vec::new();
        for (m, iface_sig) in &iface.shape.methods {
            match self.find_method(cls_name, m) {
                Some(cls_sig) if cls_sig.arity == iface_sig.arity => {
                    // Check return type mismatch when both are annotated.
                    if iface_sig.return_type != Type::Unknown
                        && cls_sig.return_type != Type::Unknown
                        && iface_sig.return_type != iface_class_type
                        && !self.is_assignable(&iface_sig.return_type, &cls_sig.return_type)
                    {
                        missing.push(format!(
                            "{m}(return type mismatch: expected `{}`, got `{}`)",
                            iface_sig.return_type.display(),
                            cls_sig.return_type.display()
                        ));
                    }
                }
                Some(cls_sig) => missing.push(format!(
                    "{m}(arity {}; expected {})",
                    cls_sig.arity, iface_sig.arity
                )),
                None => missing.push(m.clone()),
            }
        }
        for (f, iface_type) in &iface.shape.fields {
            let method_sig = self.find_method(cls_name, f);
            match self.find_field(cls_name, f) {
                Some(cls_type) if self.is_assignable(iface_type, cls_type) => {}
                Some(cls_type) => missing.push(format!(
                    "{f}: type mismatch (expected `{}`, got `{}`)",
                    iface_type.display(),
                    cls_type.display()
                )),
                None if method_sig.is_some_and(|s| s.arity == 0) => {} // property-like method satisfies field
                None if method_sig.is_some() => missing.push(format!(
                    "{f}(arity {}; expected field/property)",
                    method_sig.unwrap().arity
                )),
                None => missing.push(f.clone()),
            }
        }
        missing.sort();
        if missing.is_empty() {
            // Fallback — list every required member for context.
            iface.shape.member_names().join(", ")
        } else {
            missing.join(", ")
        }
    }

    /// Emit a [`TycError::TypeReassignMismatch`] for a reassignment that
    /// disagrees with the binding's declared type. Distinct from
    /// [`Self::mismatch`] because the diagnostic carries a second label
    /// pointing at the original declaration site and explains `mut`
    /// semantics in its help text — the previous "type mismatch:
    /// expected X, found Y" message routinely confused users who had
    /// written `mut name = …` expecting it to behave like a fresh
    /// declaration.
    fn reassign_mismatch(
        &mut self,
        name: &str,
        expected: &Type,
        actual: &Type,
        span: (usize, usize),
        decl_span: (usize, usize),
    ) {
        if self.unsafe_depth > 0 {
            return;
        }
        let length = span.1.saturating_sub(span.0).max(1);
        let decl_length = decl_span.1.saturating_sub(decl_span.0).max(1);
        self.diagnostics
            .push_error(TycError::type_reassign_mismatch(
                name,
                expected.display(),
                actual.display(),
                &self.path,
                self.source,
                span.0,
                length,
                decl_span.0,
                decl_length,
            ));
    }

    fn mismatch(&mut self, expected: &Type, actual: &Type, span: (usize, usize)) {
        if self.unsafe_depth > 0 {
            return;
        }
        let length = span.1.saturating_sub(span.0).max(1);
        // When the expected type is a known interface and the actual is a
        // class, render a structural-conformance error instead of a generic
        // type mismatch — the failure mode is "missing or incompatible
        // member", not "wrong nominal class".
        if let (Type::Class(exp_name), Type::Class(act_name)) = (expected, actual) {
            if self.interfaces.contains_key(exp_name.as_str()) {
                let missing = self.interface_missing_members(act_name, exp_name);
                self.diagnostics
                    .push_error(TycError::interface_not_conforming(
                        exp_name.clone(),
                        act_name.clone(),
                        missing,
                        &self.path,
                        self.source,
                        span.0,
                        length,
                    ));
                return;
            }
        }
        self.diagnostics.push_error(TycError::type_mismatch(
            expected.display(),
            actual.display(),
            &self.path,
            self.source,
            span.0,
            length,
        ));
    }

    /// Emit a [`TycError::ResultErrorMismatch`] diagnostic — the dedicated
    /// shape for an error-type mismatch surfaced through `?` propagation.
    fn result_error_mismatch(
        &mut self,
        expected_err: &Type,
        actual_err: &Type,
        span: (usize, usize),
    ) {
        if self.unsafe_depth > 0 {
            return;
        }
        let length = span.1.saturating_sub(span.0).max(1);
        self.diagnostics.push_error(TycError::result_error_mismatch(
            expected_err.display(),
            actual_err.display(),
            &self.path,
            self.source,
            span.0,
            length,
        ));
    }

    fn interface_isinstance(&mut self, iface: &str, span: (usize, usize)) {
        if self.unsafe_depth > 0 {
            return;
        }
        let length = span.1.saturating_sub(span.0).max(1);
        self.diagnostics.push_error(TycError::interface_isinstance(
            iface,
            &self.path,
            self.source,
            span.0,
            length,
        ));
    }

    fn nullable_use(&mut self, name: &str, expected: &Type, span: (usize, usize)) {
        if self.unsafe_depth > 0 {
            return;
        }
        let length = span.1.saturating_sub(span.0).max(1);
        self.diagnostics.push_error(TycError::nullable_use(
            name,
            expected.strip_none().display(),
            &self.path,
            self.source,
            span.0,
            length,
        ));
    }

    fn wrong_args(&mut self, name: &str, expected: usize, actual: usize, span: (usize, usize)) {
        if self.unsafe_depth > 0 {
            return;
        }
        let length = span.1.saturating_sub(span.0).max(1);
        self.diagnostics.push_error(TycError::wrong_arg_count(
            name,
            expected,
            actual,
            &self.path,
            self.source,
            span.0,
            length,
        ));
    }

    fn not_callable(&mut self, typ: &Type, span: (usize, usize)) {
        if self.unsafe_depth > 0 {
            return;
        }
        let length = span.1.saturating_sub(span.0).max(1);
        self.diagnostics.push_error(TycError::not_callable(
            typ.display(),
            &self.path,
            self.source,
            span.0,
            length,
        ));
    }

    fn non_exhaustive_match(&mut self, union_name: &str, missing: &str, span: (usize, usize)) {
        if self.unsafe_depth > 0 {
            return;
        }
        let length = span.1.saturating_sub(span.0).max(1);
        self.diagnostics.push_error(TycError::non_exhaustive_match(
            union_name,
            missing,
            &self.path,
            self.source,
            span.0,
            length,
        ));
    }

    fn typevar_bound_violation(
        &mut self,
        typevar: &str,
        actual: &str,
        bound: &str,
        span: (usize, usize),
    ) {
        if self.unsafe_depth > 0 {
            return;
        }
        let length = span.1.saturating_sub(span.0).max(1);
        self.diagnostics
            .push_error(TycError::typevar_bound_violation(
                typevar,
                actual,
                bound,
                &self.path,
                self.source,
                span.0,
                length,
            ));
    }

    /// Validate that each inferred TypeVar binding at a call site satisfies
    /// the bound declared on the function's type parameter.
    ///
    /// `fn_name` is used to look up stored bounds; when no bounds are
    /// recorded for that function this is a no-op. Violations are emitted
    /// as `tyc::typevar_bound` diagnostics at `call_span`.
    fn check_call_typevar_bounds(
        &mut self,
        fn_name: &str,
        formal_params: &[Type],
        actual_args: &[Type],
        return_type: &Type,
        expected_return: Option<&Type>,
        call_span: (usize, usize),
    ) {
        let bounds = match self.function_type_bounds.get(fn_name).cloned() {
            Some(b) if !b.is_empty() => b,
            _ => return,
        };
        // Use the same bidirectional bindings the substitution will use,
        // so a TypeVar that's only pinned by the call-site expected type
        // (no informative args) still has its declared bound enforced.
        let bindings = compute_bidirectional_bindings(
            formal_params,
            actual_args,
            return_type,
            expected_return,
        );
        let mut tv_names: Vec<&String> = bindings.keys().collect();
        tv_names.sort();
        for tv_name in tv_names {
            let inferred = &bindings[tv_name];
            if let Some(bound) = bounds.get(tv_name) {
                if !self.is_assignable(bound, inferred) {
                    self.typevar_bound_violation(
                        tv_name,
                        &inferred.display(),
                        &bound.display(),
                        call_span,
                    );
                }
            }
        }
    }
}

/// Run the type checker on `module` and return diagnostics.
pub fn check_module(
    path: impl Into<String>,
    source: &str,
    resolved: &ResolvedModule,
    module: &ModModule,
) -> Diagnostics {
    check_module_with(path, source, resolved, module, &[], &[])
}

/// Type-check a module with knowledge of which lines opened an `unsafe:`
/// block.  Diagnostics produced inside an unsafe region are suppressed:
/// `Any` is permitted to flow freely so users can interface with untyped
/// Python boundaries.  Diagnostics at the boundary (where untyped values
/// leak back out) are still produced as normal at the assignment site
/// outside the block.
///
/// `frozen_class_lines` is the preprocessor's 0-based line index list for
/// `class NAME frozen:` declarations; the checker matches these line
/// offsets against class definitions in the AST to identify frozen
/// classes, then rejects attribute writes to their instances.
pub fn check_module_with(
    path: impl Into<String>,
    source: &str,
    resolved: &ResolvedModule,
    module: &ModModule,
    unsafe_lines: &[usize],
    frozen_class_lines: &[usize],
) -> Diagnostics {
    let mut c = Checker::new(path.into(), source, resolved);
    c.unsafe_line_starts = unsafe_byte_starts(source, unsafe_lines);
    let frozen_starts = unsafe_byte_starts(source, frozen_class_lines);

    // First pass: collect class names + function signatures so forward
    // references work.
    collect_classes_and_functions(&mut c, &module.body);
    populate_frozen_classes(&mut c, &module.body, &frozen_starts);

    c.env.enter();
    // Seed module scope with collected classes/functions and resolver bindings.
    seed_env_from_scope(&mut c, 0);
    // Seed Typhon built-in names that are not declared in the source:
    // - `env` is a comptime-only function (returns str).
    // - `BaseModel` is injected by the preprocessor for `model` classes.
    // - `Ok`/`Err` may be used before the `from typhon_runtime import`
    //   injection happens (the desugar pass adds it later).
    seed_typhon_builtins(&mut c);
    for stmt in &module.body {
        check_stmt(&mut c, stmt);
    }
    c.env.leave();

    c.diagnostics
}

/// Compute the byte offset of the start of each line in `source` that was
/// recorded as an `unsafe:` header.  The resulting offsets are used by the
/// type checker to recognise lowered `if True:` statements that correspond
/// to unsafe blocks.
fn unsafe_byte_starts(source: &str, unsafe_lines: &[usize]) -> Vec<u32> {
    if unsafe_lines.is_empty() {
        return Vec::new();
    }
    let mut line_starts: Vec<usize> = vec![0];
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let mut starts: Vec<u32> = Vec::with_capacity(unsafe_lines.len());
    for &line in unsafe_lines {
        if let Some(&offset) = line_starts.get(line) {
            // Skip leading whitespace so the start matches the `if True:`
            // token's range.start exactly.
            let rest = &source[offset..];
            let lead = rest
                .bytes()
                .take_while(|&b| b == b' ' || b == b'\t')
                .count();
            starts.push((offset + lead) as u32);
        }
    }
    starts.sort_unstable();
    starts
}

/// Populate [`Checker::frozen_classes`] from the preprocessor's
/// `frozen_class_lines` metadata. Each entry in `frozen_starts` is the
/// byte offset of the first non-whitespace character on a line containing
/// a `class NAME frozen:` declaration; a class definition matches when
/// its body range covers one of those offsets.
///
/// `impl FrozenClass:` is rewritten to `class __typhon_impl_FrozenClass(object):`
/// by the preprocessor, and methods inside it bind `self` to the pseudo
/// type — not to `FrozenClass` itself. To make `self.field = ...` writes
/// inside such methods still trip the frozen check, the matching impl
/// pseudo-class is registered as frozen in the same pass.
fn populate_frozen_classes(c: &mut Checker, body: &[Stmt], frozen_starts: &[u32]) {
    if frozen_starts.is_empty() {
        return;
    }
    for stmt in body {
        if let Stmt::ClassDef(cd) = stmt {
            let class_start = u32::from(cd.range.start());
            let name_start = u32::from(cd.name.range.start());
            if frozen_starts
                .iter()
                .any(|&m| m >= class_start && m <= name_start)
            {
                let name = cd.name.as_str();
                c.frozen_classes.insert(name.to_owned());
                c.frozen_classes.insert(format!("__typhon_impl_{}", name));
            }
        }
    }
}

/// Walk an attribute-assignment target and, if its receiver resolves to a
/// frozen class, emit a [`TycError::FrozenAssign`] diagnostic. Handles
/// nested attribute access (`a.b.c = ...`) by inferring the type of the
/// immediate receiver `a.b`; chains where any inner step lands on a
/// frozen class are flagged at the outermost write. Augmented and
/// annotated forms (`Stmt::AugAssign`, `Stmt::AnnAssign`) share this
/// helper.
fn check_attr_assign_not_frozen(c: &mut Checker, target: &Expr) {
    if c.frozen_classes.is_empty() || c.unsafe_depth > 0 {
        return;
    }
    let Expr::Attribute(attr) = target else {
        return;
    };
    let recv = infer_expr(c, &attr.value);
    // Nullable receivers are already flagged with `tyc::nullable_use`;
    // we still want to surface the frozen error when narrowed forms
    // (`T | None`) hit a frozen class, so peel the optional and look at
    // the underlying class name.
    let class_name = match recv.strip_none() {
        Type::Class(name) => name,
        _ => return,
    };
    if !c.frozen_classes.contains(&class_name) {
        return;
    }
    // `impl FrozenClass: def m(self): self.x = ...` is checked via the
    // `__typhon_impl_FrozenClass` pseudo-class. The diagnostic should
    // name the class the user wrote, not the internal pseudo.
    let display_class = class_name
        .strip_prefix("__typhon_impl_")
        .unwrap_or(&class_name)
        .to_owned();
    // Point the span at the field identifier (`name` in
    // `user.identity.name = ...`) rather than the whole attribute
    // expression. Tighter spans render better in miette, and for nested
    // chains the field is the actual thing being written.
    let span_start = attr.attr.range.start().to_usize();
    let span_end = attr.attr.range.end().to_usize();
    let length = span_end.saturating_sub(span_start).max(1);
    c.diagnostics.push_error(TycError::frozen_assign(
        display_class,
        attr.attr.as_str(),
        c.path.clone(),
        c.source,
        span_start,
        length,
    ));
}

/// Declare Typhon-specific built-in names that are not present in the
/// user's source but are introduced by preprocessing or the runtime.
fn seed_typhon_builtins(c: &mut Checker) {
    // `env` — comptime function: env("NAME") or env("NAME", "default").
    // Declared variadic so the type checker accepts both 1- and 2-arg forms.
    let env_fn = Type::Function {
        params: vec![Type::Str],
        ret: Box::new(Type::Str),
        variadic: true,
    };
    c.env.declare(TypeBinding {
        name: "env".into(),
        declared: env_fn.clone(),
        narrowed: env_fn,
        span: (0, 0),
    });
    // `BaseModel` — Pydantic base class injected by the `model` preprocessor.
    c.env.declare(TypeBinding {
        name: "BaseModel".into(),
        declared: Type::Class("BaseModel".into()),
        narrowed: Type::Class("BaseModel".into()),
        span: (0, 0),
    });
    // `Ok` and `Err` — Result constructors from typhon_runtime.  Seeded here
    // so the type checker can resolve them when `from typhon_runtime import …`
    // hasn't been injected yet (it is injected at desugar time, not check time).
    c.env.declare(TypeBinding {
        name: "Ok".into(),
        declared: Type::Class("Ok".into()),
        narrowed: Type::Class("Ok".into()),
        span: (0, 0),
    });
    c.env.declare(TypeBinding {
        name: "Err".into(),
        declared: Type::Class("Err".into()),
        narrowed: Type::Class("Err".into()),
        span: (0, 0),
    });
    // `Result` — the sealed union type, also from typhon_runtime.
    c.env.declare(TypeBinding {
        name: "Result".into(),
        declared: Type::Class("Result".into()),
        narrowed: Type::Class("Result".into()),
        span: (0, 0),
    });
}

fn collect_classes_and_functions(c: &mut Checker, body: &[Stmt]) {
    // First pass: collect every class and type-alias *name* into `c.classes`
    // so the subsequent shape and signature passes can resolve nominal
    // references like `field: OtherClass`. Doing the shape collection in
    // the same pass would see an empty class list and treat every nominal
    // type as `Unknown`.
    for stmt in body {
        match stmt {
            Stmt::ClassDef(cd) => {
                let name = cd.name.as_str().to_owned();
                c.classes.push(name.clone());
                // Collect direct base class names for inheritance tracking.
                // Handle both plain `Name` bases and `Subscript` bases like
                // `list[int]` — in the latter case the base name is the subscript
                // value (e.g. `list`).
                let parents: Vec<String> = cd
                    .bases()
                    .iter()
                    .filter_map(|b| match b {
                        Expr::Name(n) => Some(n.id.as_str().to_owned()),
                        Expr::Subscript(s) => {
                            if let Expr::Name(n) = s.value.as_ref() {
                                Some(n.id.as_str().to_owned())
                            } else {
                                None
                            }
                        }
                        _ => None,
                    })
                    .collect();
                if !parents.is_empty() {
                    c.class_parents.insert(name, parents);
                }
            }
            Stmt::TypeAlias(ta) => {
                if let Expr::Name(n) = ta.name.as_ref() {
                    let union_name = n.id.as_str().to_owned();
                    c.classes.push(union_name.clone());
                    if let Some(variants) = extract_sealed_union_variants(&ta.value) {
                        c.sealed_unions.insert(union_name, variants);
                    }
                }
            }
            _ => {}
        }
    }
    let classes = c.classes.clone();
    // Second pass: now that every class name is known, collect each class's
    // member shape so interface conformance can resolve nominal types in
    // field annotations.
    for stmt in body {
        if let Stmt::ClassDef(cd) = stmt {
            let name = cd.name.as_str().to_owned();
            let shape = collect_class_shape(cd, &classes);
            if class_inherits_protocol(cd) {
                let runtime_checkable = has_runtime_checkable_decorator(&cd.decorator_list);
                c.interfaces.insert(
                    name.clone(),
                    InterfaceDecl {
                        shape: shape.clone(),
                        runtime_checkable,
                    },
                );
            }
            c.class_shapes.insert(name, shape);
        }
    }
    // Second pass (continued): fold `impl ClassName:` and `extend ClassName:`
    // contributions into the target class's shape. The preprocessor rewrites
    // both forms into a pseudo-class named `__typhon_impl_ClassName`; methods
    // defined there must count toward interface structural conformance for
    // the documented "methods live in `impl`" rule to interoperate with
    // structural typing. Without this merge, `class Button:` + `impl Button:
    // def draw() -> None` fails to satisfy `Drawable` even though the
    // cheat-sheet says it should.
    for stmt in body {
        if let Stmt::ClassDef(cd) = stmt {
            let pseudo = cd.name.as_str();
            if let Some(target) = pseudo.strip_prefix("__typhon_impl_") {
                if c.class_shapes.contains_key(target) {
                    let impl_shape = collect_class_shape(cd, &classes);
                    let target_shape = c.class_shapes.get_mut(target).expect("checked above");
                    for (m, sig) in impl_shape.methods {
                        target_shape.methods.entry(m).or_insert(sig);
                    }
                    for (f, ty) in impl_shape.fields {
                        target_shape.fields.entry(f).or_insert(ty);
                    }
                }
            }
        }
    }
    // Third pass: record function signatures (also needs the full class list).
    // Ruff folds `async def` into `Stmt::FunctionDef` with `is_async = true`,
    // so a single arm covers both sync and async forms.
    for stmt in body {
        if let Stmt::FunctionDef(f) = stmt {
            let tps = type_param_names_from(f.type_params.as_deref());
            let sig =
                function_signature(&classes, f.parameters.as_ref(), f.returns.as_deref(), &tps);
            c.function_signatures
                .insert(f.name.as_str().to_owned(), sig);
            // Record per-function arity metadata so the call-site arity
            // check can accept keyword args, defaults, `*args`, and
            // `**kwargs` without false positives (FINDINGS #44).
            c.function_arity_info.insert(
                f.name.as_str().to_owned(),
                arity_info_from_parameters(f.parameters.as_ref()),
            );
            // Also extract any declared TypeVar bounds so they can be checked
            // at call sites.
            let bounds = type_param_bounds_from(f.type_params.as_deref(), &classes);
            if !bounds.is_empty() {
                c.function_type_bounds
                    .insert(f.name.as_str().to_owned(), bounds);
            }
        }
    }
}

/// Extract the names of PEP 695 type parameters from the `Option<Box<TypeParams>>`
/// field on `StmtFunctionDef`/`StmtClassDef`. Returns an empty `Vec` when the
/// function/class has no `[T, U, ...]` clause.
fn type_param_names_from(type_params: Option<&ruff_python_ast::TypeParams>) -> Vec<String> {
    match type_params {
        Some(tps) => collect_type_param_names(&tps.type_params),
        None => Vec::new(),
    }
}

/// Extract declared bounds for PEP 695 `TypeVar` parameters.
///
/// Returns a map from typevar name to the resolved bound `Type`.  Only
/// `TypeVar` params with a bound expression are included; `ParamSpec` and
/// `TypeVarTuple` are skipped.  Bound expressions that resolve to
/// `Type::Unknown` are also skipped (unannotated or unresolvable bounds
/// carry no actionable constraint).
fn type_param_bounds_from(
    type_params: Option<&ruff_python_ast::TypeParams>,
    classes: &[String],
) -> HashMap<String, Type> {
    let Some(tps) = type_params else {
        return HashMap::new();
    };
    let mut bounds = HashMap::new();
    for tp in &tps.type_params {
        if let ruff_python_ast::TypeParam::TypeVar(tv) = tp {
            if let Some(bound_expr) = &tv.bound {
                let bound_type = type_from_annotation(bound_expr, classes);
                if bound_type != Type::Unknown {
                    bounds.insert(tv.name.as_str().to_owned(), bound_type);
                }
            }
        }
    }
    bounds
}

/// `true` if `c` lists `Protocol` in its bases.
fn class_inherits_protocol(c: &ruff_python_ast::StmtClassDef) -> bool {
    c.bases()
        .iter()
        .any(|b| matches!(b, Expr::Name(n) if n.id.as_str() == "Protocol"))
}

/// `true` if `decorators` includes `@runtime_checkable` (bare or
/// `typing.runtime_checkable`). When set, `isinstance(x, Interface)` is
/// permitted — the protocol opted in to the attribute-presence check.
fn has_runtime_checkable_decorator(decorators: &[ruff_python_ast::Decorator]) -> bool {
    decorators.iter().any(|d| {
        let name = match &d.expression {
            Expr::Name(n) => Some(n.id.as_str()),
            Expr::Attribute(a) => Some(a.attr.as_str()),
            _ => None,
        };
        name == Some("runtime_checkable")
    })
}

/// Walk a class body and record its methods and annotated fields into an
/// [`InterfaceShape`]. The receiver parameter (first positional, conventionally
/// `self` or `cls`) is excluded from the arity count.
///
/// `classes` is the module-level class list, threaded through so nominal
/// references in field annotations (`field: OtherClass`) resolve correctly
/// rather than landing as `Type::Unknown`.
fn collect_class_shape(cd: &ruff_python_ast::StmtClassDef, classes: &[String]) -> InterfaceShape {
    let mut shape = InterfaceShape::default();
    for stmt in &cd.body {
        match stmt {
            // Ruff folds `async def` into `Stmt::FunctionDef` with `is_async = true`.
            Stmt::FunctionDef(f) => {
                let arity = method_arity_excluding_receiver(f.parameters.as_ref());
                let return_type = match f.returns.as_deref() {
                    Some(r) => type_from_annotation(r, classes),
                    None => Type::Unknown,
                };
                shape
                    .methods
                    .insert(f.name.as_str().to_owned(), MethodSig { arity, return_type });
            }
            Stmt::AnnAssign(a) => {
                if let Expr::Name(n) = a.target.as_ref() {
                    let ty = type_from_annotation(&a.annotation, classes);
                    shape.fields.insert(n.id.as_str().to_owned(), ty);
                }
            }
            _ => {}
        }
    }
    shape
}

fn method_arity_excluding_receiver(params: &ruff_python_ast::Parameters) -> usize {
    let total = params.posonlyargs.len() + params.args.len() + params.kwonlyargs.len();
    // Conservatively assume one receiver (`self`/`cls`) when at least one
    // positional argument is present; static methods are uncommon enough that
    // this approximation is acceptable for v1's "member presence" check.
    total.saturating_sub(1)
}

fn function_signature(
    classes: &[String],
    parameters: &ruff_python_ast::Parameters,
    returns: Option<&Expr>,
    type_params: &[String],
) -> Type {
    let mut params = Vec::new();
    let all = parameters
        .posonlyargs
        .iter()
        .chain(parameters.args.iter())
        .chain(parameters.kwonlyargs.iter());
    for pwd in all {
        let t = match &pwd.parameter.annotation {
            Some(ann) => type_from_annotation_with_params(ann, classes, type_params),
            None => Type::Unknown,
        };
        params.push(t);
    }
    let ret = match returns {
        Some(r) => type_from_annotation_with_params(r, classes, type_params),
        None => Type::Unknown,
    };
    Type::Function {
        params,
        ret: Box::new(ret),
        // `variadic = true` when the function carries `*args`. This lets
        // the call-site arity check accept any number of positional
        // arguments beyond the declared params (FINDINGS #44c).
        variadic: parameters.vararg.is_some(),
    }
}

/// Decide whether a call site's positional + keyword arguments are
/// compatible with the named function's [`ArityInfo`]. Returns `true`
/// for an OK call, `false` for an arity mismatch — the caller emits
/// the diagnostic.
///
/// Rules:
/// 1. Every kw argument must either match a parameter name (positional
///    or kw-only), or be absorbed by `**kwargs`. Mismatch → `false`.
/// 2. A parameter can be filled by either a positional or a kw arg —
///    not both. Conflicts → `false`.
/// 3. Every required positional / kw-only param must be filled.
/// 4. The total positional count must not exceed `max_positional`
///    (unless the function has `*args`, in which case `max_positional`
///    is `None`).
fn check_arity_with_info(
    info: &ArityInfo,
    pos_args: &[Expr],
    kw_args: &[ruff_python_ast::Keyword],
) -> bool {
    // Filter out the `**double-star` unpacking keywords (`kw.arg == None`) — we
    // can't statically know how many keys they contain, so we treat them as
    // matching anything (kwarg sentinel).
    let named_kwargs: Vec<&str> = kw_args
        .iter()
        .filter_map(|k| k.arg.as_ref().map(|i| i.as_str()))
        .collect();
    let has_double_star = kw_args.iter().any(|k| k.arg.is_none());

    // Rule 4: positional count must fit max_positional (None → unbounded).
    if let Some(max) = info.max_positional {
        if pos_args.len() > max {
            return false;
        }
    }

    // Rule 1: every named kw must hit a parameter (or `**kwargs`).
    if !info.has_kwarg {
        for name in &named_kwargs {
            let hits_pos = info.param_names.iter().any(|p| p == name);
            let hits_kwonly = info.kwonly_names.iter().any(|p| p == name);
            if !hits_pos && !hits_kwonly {
                return false;
            }
        }
    }

    // Rule 2: a positional-bound name can't also appear as a kw.
    let filled_positionally = pos_args.len().min(info.param_names.len());
    for name in &named_kwargs {
        if info.param_names[..filled_positionally].iter().any(|p| p == name) {
            return false;
        }
    }

    // Rule 3a: every required positional must be filled by a pos arg or
    // matching kw arg. Stops being checkable when `**kwargs` unpacking is
    // present — in that case we trust the user.
    if !has_double_star {
        for (i, p) in info.param_names.iter().enumerate().take(info.min_positional) {
            if i < pos_args.len() {
                continue;
            }
            if named_kwargs.iter().any(|kw| kw == p) {
                continue;
            }
            return false;
        }
        // Rule 3b: every required kw-only must be filled.
        for required in &info.kwonly_required {
            if !named_kwargs.iter().any(|kw| kw == required) {
                return false;
            }
        }
    }
    true
}

/// Compute the [`ArityInfo`] sidecar for a `def`'s parameter list.
///
/// Walks the same positional / keyword / vararg / kwarg shape as
/// `function_signature` but extracts the metadata that doesn't fit on
/// `Type::Function` (param names for keyword-arg matching, count of
/// defaulted params for the min-arity bound, kw-only requireds, and
/// the `**kwargs` flag).
fn arity_info_from_parameters(parameters: &ruff_python_ast::Parameters) -> ArityInfo {
    let mut param_names: Vec<String> = Vec::new();
    let mut min_positional: usize = 0;
    // Walk positional-only + positional-or-keyword. A defaulted positional
    // doesn't count toward `min_positional`; once we see the first
    // defaulted param all subsequent positionals must also be defaulted
    // (Python grammar enforces this), so we can stop incrementing once
    // we encounter a default.
    let positional_chain = parameters.posonlyargs.iter().chain(parameters.args.iter());
    let mut hit_default = false;
    let mut max_positional_count: usize = 0;
    for pwd in positional_chain {
        param_names.push(pwd.parameter.name.as_str().to_owned());
        max_positional_count += 1;
        if pwd.default.is_none() && !hit_default {
            min_positional += 1;
        } else {
            hit_default = true;
        }
    }
    let max_positional = if parameters.vararg.is_some() {
        None
    } else {
        Some(max_positional_count)
    };
    let mut kwonly_names: Vec<String> = Vec::new();
    let mut kwonly_required: Vec<String> = Vec::new();
    for pwd in &parameters.kwonlyargs {
        let name = pwd.parameter.name.as_str().to_owned();
        kwonly_names.push(name.clone());
        if pwd.default.is_none() {
            kwonly_required.push(name);
        }
    }
    ArityInfo {
        param_names,
        min_positional,
        max_positional,
        kwonly_names,
        kwonly_required,
        has_kwarg: parameters.kwarg.is_some(),
    }
}

fn seed_env_from_scope(c: &mut Checker, scope: ScopeId) {
    // Use the resolver's bindings as the seed of declared names. Annotation
    // types will be filled in as we encounter the actual AnnAssign / def.
    // Here we register classes (as Type::Class) and functions (with their
    // computed signatures).
    let bindings: Vec<Binding> = c.resolved.scopes[scope].bindings.clone();
    for b in bindings {
        let declared = match b.kind {
            BindingKind::Class => Type::Class(b.name.clone()),
            BindingKind::Function => c
                .function_signatures
                .get(&b.name)
                .cloned()
                .unwrap_or(Type::Unknown),
            _ => Type::Unknown,
        };
        c.env.declare(TypeBinding {
            name: b.name,
            declared: declared.clone(),
            narrowed: declared,
            span: b.span,
        });
    }
}

fn check_stmt(c: &mut Checker, stmt: &Stmt) {
    match stmt {
        Stmt::AnnAssign(a) => {
            let ann_type = type_from_annotation(&a.annotation, &c.classes);
            if let Some(value) = &a.value {
                let value_type = infer_expr_ctx(c, value, Some(&ann_type));
                if !c.is_assignable(&ann_type, &value_type) {
                    let span = (
                        value.range().start().to_usize(),
                        value.range().end().to_usize(),
                    );
                    c.mismatch(&ann_type, &value_type, span);
                }
                // `a.b: T = ...` — uncommon, but still a write to `a.b`.
                // Field declarations inside a class body have no value
                // and a bare-name target, so they don't hit this branch.
                check_attr_assign_not_frozen(c, a.target.as_ref());
            }
            if let Expr::Name(n) = a.target.as_ref() {
                let span = (
                    n.range.start().to_usize(),
                    n.range.start().to_usize() + n.id.as_str().len(),
                );
                c.env.declare(TypeBinding {
                    name: n.id.as_str().to_owned(),
                    declared: ann_type.clone(),
                    narrowed: ann_type,
                    span,
                });
            }
        }
        Stmt::Assign(a) => {
            let value_type = infer_expr(c, &a.value);
            for target in &a.targets {
                check_attr_assign_not_frozen(c, target);
                if let Expr::Name(n) = target {
                    let span = (
                        n.range.start().to_usize(),
                        n.range.start().to_usize() + n.id.as_str().len(),
                    );
                    let existing = c.env.lookup(n.id.as_str()).cloned();
                    if let Some(b) = existing {
                        // Reassignment: the static type stays as declared;
                        // check the new value fits. When it doesn't, emit
                        // the dedicated reassignment-mismatch diagnostic
                        // so the user sees both the offending value AND
                        // the original declaration site (with help text
                        // that explains `mut` allows new values of the
                        // same type, not a new type).
                        if !c.is_assignable(&b.declared, &value_type) {
                            let vspan = (
                                a.value.range().start().to_usize(),
                                a.value.range().end().to_usize(),
                            );
                            c.reassign_mismatch(
                                n.id.as_str(),
                                &b.declared,
                                &value_type,
                                vspan,
                                b.span,
                            );
                        }
                        // Reset any narrowing on the reassigned name.
                        c.env.narrow(n.id.as_str(), b.declared);
                    } else {
                        c.env.declare(TypeBinding {
                            name: n.id.as_str().to_owned(),
                            declared: value_type.clone(),
                            narrowed: value_type.clone(),
                            span,
                        });
                    }
                }
            }
        }
        Stmt::FunctionDef(f) => {
            let tps = type_param_names_from(f.type_params.as_deref());
            let bounds = type_param_bounds_from(f.type_params.as_deref(), &c.classes.clone());
            if !bounds.is_empty() {
                c.function_type_bounds
                    .insert(f.name.as_str().to_owned(), bounds);
            }
            check_function(
                c,
                f.name.as_str(),
                f.parameters.as_ref(),
                &f.body,
                f.returns.as_deref(),
                &tps,
            )
        }
        Stmt::ClassDef(cd) => {
            // FINDINGS #17: nudge users toward `impl ClassName:` when they
            // put a `def` inside `class ClassName:`. Skipped for impl
            // pseudo-classes (`__typhon_impl_*`), Protocols (interfaces),
            // Pydantic models, and the lazy/builtin-extend stubs — those
            // legitimately carry method definitions inside the class body.
            let class_name = cd.name.as_str();
            let is_pseudo =
                class_name.starts_with("__typhon_") || class_name.starts_with("__TyphonLazy_");
            let is_protocol = class_inherits_protocol(cd);
            // Mirror the desugar pass's heuristic: a `model X:` becomes
            // `class X(BaseModel):` after preprocess. Looking for any
            // base named `BaseModel` is good enough for this warning.
            let is_pydantic = cd.bases().iter().any(|b| match b {
                Expr::Name(n) => n.id.as_str() == "BaseModel",
                _ => false,
            });
            if !is_pseudo && !is_protocol && !is_pydantic {
                for s in &cd.body {
                    if let Stmt::FunctionDef(f) = s {
                        let method = f.name.as_str();
                        // Don't warn on dunders the user is *expected* to
                        // override (e.g. `__add__`, `__lt__`); those are
                        // legitimate uses of class-body methods too. The
                        // canonical bad case is user-named methods like
                        // `draw`, `display`, `is_admin`.
                        if method.starts_with("__") && method.ends_with("__") {
                            continue;
                        }
                        let span_start = f.name.range.start().to_usize();
                        c.diagnostics.push_warning(TycError::method_in_class_body(
                            class_name.to_owned(),
                            method.to_owned(),
                            c.path.clone(),
                            c.source,
                            span_start,
                            method.len().max(1),
                        ));
                    }
                }
            }
            c.env.enter();
            let saved_class = c.current_class.replace(cd.name.as_str().to_owned());
            for s in &cd.body {
                check_stmt(c, s);
            }
            c.current_class = saved_class;
            c.env.leave();
        }
        Stmt::Return(ret) => {
            if let (Some(ret_expr), Some(expected)) = (&ret.value, c.current_return.clone()) {
                let value_type = infer_expr_ctx(c, ret_expr, Some(&expected));
                if !matches!(expected, Type::Unknown) && !c.is_assignable(&expected, &value_type) {
                    let span = (
                        ret_expr.range().start().to_usize(),
                        ret_expr.range().end().to_usize(),
                    );
                    // When `?` is desugared, the synthesised return reads
                    // `return __typhon_q_N__` where the value type is
                    // `Generic("Err", [E_callee])`. Surface this as a
                    // dedicated `tyc::result_error_mismatch` so the user
                    // sees a `?`-propagation framing instead of a generic
                    // type mismatch (FINDINGS #13).
                    if is_question_op_temp(ret_expr) {
                        if let (Some(expected_err), Some(actual_err)) = (
                            extract_result_error_type(&expected),
                            extract_err_generic_param(&value_type),
                        ) {
                            if !c.is_assignable(&expected_err, &actual_err) {
                                c.result_error_mismatch(&expected_err, &actual_err, span);
                                return;
                            }
                        }
                    }
                    c.mismatch(&expected, &value_type, span);
                }
            } else if ret.value.is_none() {
                if let Some(expected) = c.current_return.clone() {
                    if !matches!(expected, Type::Unknown)
                        && !c.is_assignable(&expected, &Type::None)
                    {
                        let span = (ret.range.start().to_usize(), ret.range.end().to_usize());
                        c.mismatch(&expected, &Type::None, span);
                    }
                }
            }
        }
        Stmt::If(i) => check_if(c, i),
        Stmt::While(w) => {
            let _ = infer_expr(c, &w.test);
            for s in &w.body {
                check_stmt(c, s);
            }
            for s in &w.orelse {
                check_stmt(c, s);
            }
        }
        Stmt::For(f) => {
            let _ = infer_expr(c, &f.iter);
            if let Expr::Name(n) = f.target.as_ref() {
                let span = (
                    n.range.start().to_usize(),
                    n.range.start().to_usize() + n.id.as_str().len(),
                );
                c.env.declare(TypeBinding {
                    name: n.id.as_str().to_owned(),
                    declared: Type::Unknown,
                    narrowed: Type::Unknown,
                    span,
                });
            }
            for s in &f.body {
                check_stmt(c, s);
            }
        }
        Stmt::Expr(e) => {
            let _ = infer_expr(c, &e.value);
        }
        Stmt::AugAssign(a) => {
            let _ = infer_expr(c, &a.target);
            let _ = infer_expr(c, &a.value);
            check_attr_assign_not_frozen(c, &a.target);
        }
        Stmt::With(w) => {
            for item in &w.items {
                let _ = infer_expr(c, &item.context_expr);
            }
            for s in &w.body {
                check_stmt(c, s);
            }
        }
        Stmt::Try(t) => {
            for s in &t.body {
                check_stmt(c, s);
            }
            for h in &t.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                for s in &h.body {
                    check_stmt(c, s);
                }
            }
            for s in &t.orelse {
                check_stmt(c, s);
            }
            for s in &t.finalbody {
                check_stmt(c, s);
            }
        }
        Stmt::Match(m) => {
            let subject_type = infer_expr(c, &m.subject);
            for case in &m.cases {
                // Enter scope and bind pattern names FIRST so guard expressions
                // (e.g. `case Circle(radius=r) if r > 0:`) can reference them.
                c.env.enter();
                bind_pattern_names(c, &case.pattern);
                if let Some(guard) = &case.guard {
                    let _ = infer_expr(c, guard);
                }
                for s in &case.body {
                    check_stmt(c, s);
                }
                c.env.leave();
            }
            // Exhaustiveness check: only applies to sealed unions.
            if let Type::Class(ref union_name) = subject_type {
                if let Some(variants) = c.sealed_unions.get(union_name.as_str()).cloned() {
                    let subject_span = (
                        m.subject.range().start().to_usize(),
                        m.subject.range().end().to_usize(),
                    );
                    check_match_exhaustiveness(c, &m.cases, union_name, &variants, subject_span);
                }
            }
        }
        _ => {}
    }
}

/// Enforce Rule 1: every parameter and return type must be annotated.
/// The first positional named `self` or `cls` is exempted (the desugarer
/// inserts unannotated receivers for `impl` methods, and explicit
/// `self`/`cls` is idiomatic Python). `*args` / `**kwargs` are likewise
/// not required to carry annotations.
fn enforce_annotation_rule(
    c: &mut Checker,
    function: &str,
    parameters: &ruff_python_ast::Parameters,
    returns: Option<&Expr>,
) {
    // Skip the leading `self` / `cls` receiver, if present, so methods
    // continue to pass without annotating it.
    let positional: Vec<&ruff_python_ast::ParameterWithDefault> = parameters
        .posonlyargs
        .iter()
        .chain(parameters.args.iter())
        .collect();
    let mut params_iter = positional.iter().peekable();
    if let Some(first) = params_iter.peek() {
        let name = first.parameter.name.as_str();
        if name == "self" || name == "cls" {
            params_iter.next();
        }
    }
    for pwd in params_iter {
        if pwd.parameter.annotation.is_none() {
            let pname = pwd.parameter.name.as_str();
            let span = (
                pwd.parameter.range.start().to_usize(),
                pwd.parameter.range.start().to_usize() + pname.len(),
            );
            c.diagnostics.push_error(TycError::missing_annotation(
                function.to_owned(),
                format!("parameter `{}`", pname),
                c.path.clone(),
                c.source,
                span.0,
                pname.len().max(1),
            ));
        }
    }
    for pwd in parameters.kwonlyargs.iter() {
        if pwd.parameter.annotation.is_none() {
            let pname = pwd.parameter.name.as_str();
            let span = (
                pwd.parameter.range.start().to_usize(),
                pwd.parameter.range.start().to_usize() + pname.len(),
            );
            c.diagnostics.push_error(TycError::missing_annotation(
                function.to_owned(),
                format!("parameter `{}`", pname),
                c.path.clone(),
                c.source,
                span.0,
                pname.len().max(1),
            ));
        }
    }

    if returns.is_none() {
        // Anchor the diagnostic on the function name. Spans for the
        // closing-paren / arrow position aren't easily reachable from
        // `ruff_python_ast::Parameters`; the name is unambiguous and
        // matches the user's mental model ("annotate this function").
        let span_start = parameters.range.start().to_usize();
        c.diagnostics.push_error(TycError::missing_annotation(
            function.to_owned(),
            "return type".to_owned(),
            c.path.clone(),
            c.source,
            span_start,
            1,
        ));
    }
}

fn check_function(
    c: &mut Checker,
    name: &str,
    parameters: &ruff_python_ast::Parameters,
    body: &[Stmt],
    returns: Option<&Expr>,
    type_params: &[String],
) {
    let classes = c.classes.clone();
    let ret_type = match returns {
        Some(r) => type_from_annotation_with_params(r, &classes, type_params),
        None => Type::Unknown,
    };

    // Rule 1: every parameter and return type must be annotated.
    // Auto-synthesised compiler helpers (anything `__typhon_*`) are
    // exempted so the desugar pass can keep emitting unannotated
    // bridges without provoking the user-facing diagnostic.
    if !name.starts_with("__typhon_") {
        enforce_annotation_rule(c, name, parameters, returns);
    }

    let saved_return = c.current_return.replace(ret_type);
    // Load the TypeVar bounds for this function so the body's attribute
    // accesses (e.g. `x.greet()` where `x: T` and `T: Greeter`) can resolve
    // against the bound's interface shape.
    let saved_bounds = std::mem::take(&mut c.active_typevar_bounds);
    c.active_typevar_bounds = c
        .function_type_bounds
        .get(name)
        .cloned()
        .unwrap_or_default();
    c.env.enter();

    // Declare parameters with their annotation types. Type parameters resolve
    // to `Any` until a real inference engine lands. Inside a class body, an
    // unannotated leading `self` parameter inherits the enclosing class
    // type so writes like `self.field = ...` participate in field-level
    // checks (currently: the frozen-class rejection).
    let positional_first_name = parameters
        .posonlyargs
        .first()
        .or_else(|| parameters.args.first())
        .map(|p| p.parameter.name.as_str().to_owned());
    let all = parameters
        .posonlyargs
        .iter()
        .chain(parameters.args.iter())
        .chain(parameters.kwonlyargs.iter());
    for pwd in all {
        let param_name = pwd.parameter.name.as_str();
        let is_self_receiver = positional_first_name.as_deref() == Some(param_name)
            && (param_name == "self" || param_name == "cls");
        let t = match &pwd.parameter.annotation {
            Some(ann) => type_from_annotation_with_params(ann, &classes, type_params),
            None => {
                if is_self_receiver {
                    // `extend BUILTIN:` is lowered to a
                    // `__typhon_builtin_ext_*` sentinel class whose methods
                    // are later extracted to free functions; `self` there
                    // morally has the builtin's type, not the sentinel's.
                    // Leave it `Unknown` until the extraction pass can
                    // supply the real receiver type.
                    let in_builtin_ext = c
                        .current_class
                        .as_deref()
                        .is_some_and(|n| n.starts_with("__typhon_builtin_ext_"));
                    if in_builtin_ext {
                        Type::Unknown
                    } else {
                        match c.current_class.as_deref() {
                            Some(cls) => Type::Class(cls.to_owned()),
                            None => Type::Unknown,
                        }
                    }
                } else {
                    Type::Unknown
                }
            }
        };
        let span = (
            pwd.parameter.range.start().to_usize(),
            pwd.parameter.range.start().to_usize() + param_name.len(),
        );
        c.env.declare(TypeBinding {
            name: param_name.to_owned(),
            declared: t.clone(),
            narrowed: t,
            span,
        });
    }

    for stmt in body {
        check_stmt(c, stmt);
    }

    c.env.leave();
    c.current_return = saved_return;
    c.active_typevar_bounds = saved_bounds;
}

fn check_if(c: &mut Checker, i: &ruff_python_ast::StmtIf) {
    // Recognise the `if True:` form that the preprocessor emits for an
    // `unsafe:` block.  Inside the body, type-check diagnostics are
    // suppressed; the `else` branch is empty in practice (the preprocessor
    // never emits one) but is handled safely below.
    let is_unsafe = c.is_unsafe_marker(i.range);

    let _ = infer_expr(c, &i.test);

    // Apply narrowing for the true branch.
    let narrowings = collect_narrowings(c, &i.test, /*negate=*/ false);
    let snap_pre = c.env.snapshot();
    apply_narrowings(c, &narrowings);
    if is_unsafe {
        c.unsafe_depth = c.unsafe_depth.saturating_add(1);
    }
    for s in &i.body {
        check_stmt(c, s);
    }
    if is_unsafe {
        c.unsafe_depth = c.unsafe_depth.saturating_sub(1);
    }
    let body_exits = body_always_exits(&i.body);
    c.env.restore(snap_pre);

    // Apply opposite narrowing for the elif/else cascade. Ruff flattens
    // `if/elif/else` into `elif_else_clauses` — walk them as a virtual
    // chain so each elif gets positive narrowing of its own test on top of
    // the cumulative negation of every earlier test.
    let neg = collect_narrowings(c, &i.test, /*negate=*/ true);
    let snap_pre = c.env.snapshot();
    apply_narrowings(c, &neg);
    check_elif_else_clauses(c, &i.elif_else_clauses);
    let elif_exits =
        !i.elif_else_clauses.is_empty() && elif_else_chain_always_exits(&i.elif_else_clauses);
    c.env.restore(snap_pre);

    // Flow-sensitive narrowing: if every branch we've examined ends in
    // an unconditional exit (return / raise / break / continue), the
    // negated narrowing should persist into the surrounding scope. This
    // makes the `guard X = expr else: return` lowering work — after the
    // None-check, `expr` is reliably non-None for the rest of the body.
    if body_exits && (i.elif_else_clauses.is_empty() || elif_exits) {
        apply_narrowings(c, &neg);
    }
}

/// True when every reachable path through `stmts` exits the enclosing
/// function (return / raise / break / continue) before falling off the
/// end. Used by `check_if` to decide whether to propagate the negated
/// narrowing of the `if`'s test into the post-`if` scope.
fn body_always_exits(stmts: &[Stmt]) -> bool {
    stmts.last().is_some_and(stmt_always_exits)
}

/// True when every branch of an elif/else chain always exits.
fn elif_else_chain_always_exits(clauses: &[ruff_python_ast::ElifElseClause]) -> bool {
    // The chain "always exits" only when there is a terminal `else:` and
    // every elif/else body always exits. Without a terminal `else`, the
    // fall-through path is "do nothing" — not an exit.
    let has_terminal_else = clauses.iter().any(|c| c.test.is_none());
    if !has_terminal_else {
        return false;
    }
    clauses.iter().all(|c| body_always_exits(&c.body))
}

/// True when this statement unconditionally exits its enclosing scope.
fn stmt_always_exits(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Return(_) | Stmt::Raise(_) | Stmt::Break(_) | Stmt::Continue(_) => true,
        // An if/elif/else where every branch exits is itself an exit.
        Stmt::If(s) => {
            body_always_exits(&s.body) && elif_else_chain_always_exits(&s.elif_else_clauses)
        }
        _ => false,
    }
}

/// Recursively check the `elif`/`else` cascade of a [`StmtIf`].  Each
/// clause's `test` is `None` for `else`, `Some(_)` for `elif`.  An `elif`
/// behaves like a nested `if` whose true branch narrows positively and whose
/// false branch (the remaining clauses) narrows negatively.
fn check_elif_else_clauses(c: &mut Checker, clauses: &[ruff_python_ast::ElifElseClause]) {
    let Some((first, rest)) = clauses.split_first() else {
        return;
    };
    match &first.test {
        Some(test) => {
            // elif: nested-if semantics on top of the already-negated outer test.
            let _ = infer_expr(c, test);
            let pos = collect_narrowings(c, test, /*negate=*/ false);
            let snap = c.env.snapshot();
            apply_narrowings(c, &pos);
            for s in &first.body {
                check_stmt(c, s);
            }
            c.env.restore(snap);

            let neg = collect_narrowings(c, test, /*negate=*/ true);
            let snap = c.env.snapshot();
            apply_narrowings(c, &neg);
            check_elif_else_clauses(c, rest);
            c.env.restore(snap);
        }
        None => {
            // else: just check the body in the current narrowed env.
            for s in &first.body {
                check_stmt(c, s);
            }
        }
    }
}

/// A narrowing instruction: replace the narrowed type of `name` with
/// `replacement` (subject to it being compatible with the declared type).
#[derive(Debug, Clone)]
struct Narrowing {
    name: String,
    replacement: Type,
}

/// Collect narrowings implied by `test`. If `negate` is true, invert the
/// sense (used for the `else` branch).
fn collect_narrowings(c: &Checker, test: &Expr, negate: bool) -> Vec<Narrowing> {
    let mut out = Vec::new();
    collect_narrowings_inner(c, test, negate, &mut out);
    out
}

fn collect_narrowings_inner(c: &Checker, test: &Expr, negate: bool, out: &mut Vec<Narrowing>) {
    match test {
        Expr::Compare(cmp) => {
            // x is None / x is not None
            if cmp.ops.len() == 1 && cmp.comparators.len() == 1 {
                let is_op = matches!(cmp.ops[0], ruff_python_ast::CmpOp::Is);
                let is_not_op = matches!(cmp.ops[0], ruff_python_ast::CmpOp::IsNot);
                if is_op || is_not_op {
                    if let (Expr::Name(n), Expr::NoneLiteral(_)) =
                        (cmp.left.as_ref(), &cmp.comparators[0])
                    {
                        if let Some(b) = c.env.lookup(n.id.as_str()) {
                            // x is None  → name becomes None
                            // x is not None → name becomes declared without None
                            let positive_match = is_op; // is None
                            let want_none = if negate {
                                !positive_match
                            } else {
                                positive_match
                            };
                            let replacement = if want_none {
                                Type::None
                            } else {
                                b.narrowed.strip_none()
                            };
                            out.push(Narrowing {
                                name: n.id.as_str().to_owned(),
                                replacement,
                            });
                        }
                    }
                }
            }
        }
        Expr::Call(call) => {
            // isinstance(x, T)
            if let Expr::Name(fn_name) = call.func.as_ref() {
                let pos_args = &call.arguments.args;
                if fn_name.id.as_str() == "isinstance" && pos_args.len() == 2 {
                    if let Expr::Name(target) = &pos_args[0] {
                        let new_type = type_from_annotation(&pos_args[1], &c.classes);
                        if let Some(b) = c.env.lookup(target.id.as_str()) {
                            let replacement = if negate {
                                // Best-effort: strip the type out of the union.
                                strip_variant(&b.narrowed, &new_type)
                            } else {
                                // Preserve generic parameters when narrowing
                                // `Result[T, E]` against `Ok` or `Err`.
                                // Without this, the post-`?`-operator
                                // `return x` (where `x` is now `Class("Err")`)
                                // would lose `E` and the
                                // `result_error_mismatch` check (FINDINGS #13)
                                // can't fire.
                                refine_isinstance_target(&b.narrowed, &new_type)
                            };
                            out.push(Narrowing {
                                name: target.id.as_str().to_owned(),
                                replacement,
                            });
                        }
                    }
                }
            }
        }
        Expr::UnaryOp(u) if matches!(u.op, ruff_python_ast::UnaryOp::Not) => {
            collect_narrowings_inner(c, &u.operand, !negate, out);
        }
        Expr::Name(n) => {
            // Truthy narrowing: `if x:` strips None from `x` in the true
            // branch (truthy implies not None). The else branch isn't
            // narrowed in the opposite direction because falsy doesn't
            // imply None — `int? == 0` is also falsy and stays nullable.
            if !negate {
                if let Some(b) = c.env.lookup(n.id.as_str()) {
                    if b.narrowed.is_nullable() {
                        out.push(Narrowing {
                            name: n.id.as_str().to_owned(),
                            replacement: b.narrowed.strip_none(),
                        });
                    }
                }
            }
        }
        _ => {}
    }
}

/// Return the [`Type::Function`] signature for a built-in method on a
/// generic container, or `None` when the receiver/method combo isn't a
/// known intrinsic.
///
/// Today this covers the cases the FINDINGS doc called out — primarily
/// `dict.get(k)` which must return `V?`, not `V` — and is the right
/// place to grow other built-in method types (`list.pop`, `str.find`,
/// `re.match`, …) without scattering them across the inference engine.
fn builtin_generic_method(recv: &Type, attr: &str) -> Option<Type> {
    let Type::Generic(head, args) = recv else {
        return None;
    };
    match (head.as_str(), attr, args.as_slice()) {
        ("dict", "get", [k, v]) => {
            // `d.get(k)` → V?  ;  `d.get(k, default)` → also typed as V?
            // (the default may broaden the runtime type, but the static
            // contract Typhon advertises is "V?"). Variadic so 1- and
            // 2-arg call sites both pass the arity check.
            let ret = Type::union_of(vec![v.clone(), Type::None]);
            Some(Type::Function {
                params: vec![k.clone()],
                ret: Box::new(ret),
                variadic: true,
            })
        }
        _ => None,
    }
}

/// Refine the type chosen by an `isinstance(x, T)` narrowing so that
/// generic parameters survive when the source `x: Result[A, B]` is
/// narrowed against the bare `Err` / `Ok` constructor. Without this
/// the post-`isinstance` `return x` from a `?`-operator expansion
/// would have type `Class("Err")` and the
/// `result_error_mismatch` check wouldn't see `Err[E]` vs
/// `Result[T, E_outer]`.
/// `true` when `expr` is a bare reference to a `?`-operator temporary
/// (`__typhon_q_N__`). These names are synthesised by the preprocessor's
/// [`tyc_syntax::expand_question_ops`] pass; a `return __typhon_q_N__`
/// statement is therefore guaranteed to come from a `?` lowering, never
/// from user-written code (the leading double underscore is reserved).
fn is_question_op_temp(expr: &Expr) -> bool {
    if let Expr::Name(n) = expr {
        let s = n.id.as_str();
        return s.starts_with("__typhon_q_") && s.ends_with("__");
    }
    false
}

/// Extract `E` from a `Result[T, E]` type. Returns `None` for any other
/// shape — including the bare `Result` class (which would carry no
/// information for the diagnostic).
fn extract_result_error_type(typ: &Type) -> Option<Type> {
    if let Type::Generic(name, args) = typ {
        if name == "Result" && args.len() == 2 {
            return Some(args[1].clone());
        }
    }
    None
}

/// Extract `E` from a `Generic("Err", [E])` type as produced by
/// [`refine_isinstance_target`] when narrowing a `Result[T, E]` against
/// `Err`. Returns `None` for `Class("Err")` (no generic parameter
/// surviving) or any unrelated type.
fn extract_err_generic_param(typ: &Type) -> Option<Type> {
    if let Type::Generic(name, args) = typ {
        if name == "Err" && args.len() == 1 {
            return Some(args[0].clone());
        }
    }
    None
}

fn refine_isinstance_target(current: &Type, narrowed_to: &Type) -> Type {
    let current_generic = match current {
        Type::Generic(name, args) if name == "Result" && args.len() == 2 => Some(args),
        _ => None,
    };
    let narrowed_class = match narrowed_to {
        Type::Class(name) if name == "Ok" || name == "Err" => Some(name.as_str()),
        _ => None,
    };
    if let (Some(args), Some(class)) = (current_generic, narrowed_class) {
        let param = match class {
            "Ok" => args[0].clone(),
            "Err" => args[1].clone(),
            _ => unreachable!(),
        };
        return Type::Generic(class.to_owned(), vec![param]);
    }
    narrowed_to.clone()
}

fn strip_variant(typ: &Type, variant: &Type) -> Type {
    if let Type::Union(xs) = typ {
        let kept: Vec<Type> = xs.iter().filter(|t| *t != variant).cloned().collect();
        Type::union_of(kept)
    } else if typ == variant {
        Type::Unknown
    } else {
        typ.clone()
    }
}

fn apply_narrowings(c: &mut Checker, ns: &[Narrowing]) {
    for n in ns {
        c.env.narrow(&n.name, n.replacement.clone());
    }
}

/// Infer the type of an expression.
/// Infer the type of `expr` with no surrounding context.
fn infer_expr(c: &mut Checker, expr: &Expr) -> Type {
    infer_expr_ctx(c, expr, None)
}

/// Infer the type of `expr`, optionally with an expected target type
/// (the annotation on the enclosing `let`, the function's declared
/// return type, or a generic parameter's formal type).  Most arms
/// ignore `expected`; it's used for:
///
/// - **`Expr::Call`** — feeds into PEP 695 bidirectional inference so
///   call-site annotations can pin otherwise-unbindable TypeVars.
/// - **`Expr::List` / `Expr::Set` / `Expr::Dict`** — when the literal
///   is empty (no elements to infer from), adopt the expected element
///   type so `let xs: list[int] = []` produces `list[int]` rather than
///   `list[?]`.
fn infer_expr_ctx(c: &mut Checker, expr: &Expr, expected: Option<&Type>) -> Type {
    match expr {
        Expr::BooleanLiteral(_) => Type::Bool,
        Expr::NumberLiteral(n) => match &n.value {
            Number::Int(_) => Type::Int,
            Number::Float(_) => Type::Float,
            Number::Complex { .. } => Type::Unknown,
        },
        Expr::StringLiteral(_) => Type::Str,
        Expr::BytesLiteral(_) => Type::Bytes,
        Expr::NoneLiteral(_) => Type::None,
        Expr::Name(n) => {
            if let Some(b) = c.env.lookup(n.id.as_str()) {
                b.narrowed.clone()
            } else {
                Type::Unknown
            }
        }
        Expr::BinOp(b) => {
            let l = infer_expr(c, &b.left);
            let r = infer_expr(c, &b.right);
            // Possibly-None operands can't participate in arithmetic — flag
            // the offending name so the user knows to guard with
            // `if x is not None:`.
            for (side, ty) in [(b.left.as_ref(), &l), (b.right.as_ref(), &r)] {
                if ty.is_nullable() {
                    if let Expr::Name(n) = side {
                        let span = (
                            n.range.start().to_usize(),
                            n.range.start().to_usize() + n.id.as_str().len(),
                        );
                        c.nullable_use(n.id.as_str(), ty, span);
                    }
                }
            }
            // Conservative numeric arithmetic inference.
            match (&l.strip_none(), &r.strip_none()) {
                (Type::Int, Type::Int) => Type::Int,
                (Type::Float, _) | (_, Type::Float) => Type::Float,
                (Type::Str, Type::Str) if matches!(b.op, Operator::Add) => Type::Str,
                _ => Type::Unknown,
            }
        }
        Expr::BoolOp(_) => Type::Bool,
        Expr::Compare(_) => Type::Bool,
        Expr::UnaryOp(u) => {
            let operand = infer_expr(c, &u.operand);
            match u.op {
                // Boolean negation always produces a bool, regardless of
                // operand type (Python: `not x` returns a bool).
                ruff_python_ast::UnaryOp::Not => Type::Bool,
                // Bitwise / arithmetic unary ops preserve the operand type.
                _ => operand,
            }
        }
        Expr::Call(call) => {
            // Ruff folds positional and keyword arguments into `call.arguments`.
            let pos_args = &call.arguments.args;
            let kw_args = &call.arguments.keywords;
            // Ok(value) and Err(error) are Result constructors: infer their
            // types as Generic("Ok", [T]) / Generic("Err", [E]) so that the
            // Result assignability rule in `assignable` can fire.
            if let Expr::Name(fn_name) = call.func.as_ref() {
                let ctor = fn_name.id.as_str();
                if (ctor == "Ok" || ctor == "Err") && pos_args.len() == 1 && kw_args.is_empty() {
                    let arg_type = infer_expr(c, &pos_args[0]);
                    return Type::Generic(ctor.to_owned(), vec![arg_type]);
                }
                // isinstance(x, Interface) is rejected unless the interface
                // explicitly opts in via @runtime_checkable. Runtime Protocol
                // isinstance only checks attribute *presence*, not signature,
                // so we reject the static use by default. Interfaces decorated
                // `@runtime_checkable` are exempt — the user acknowledged the
                // weaker guarantee.
                if fn_name.id.as_str() == "isinstance" && pos_args.len() == 2 {
                    if let Expr::Name(t) = &pos_args[1] {
                        if let Some(iface) = c.interfaces.get(t.id.as_str()) {
                            if !iface.runtime_checkable {
                                let span = (
                                    t.range.start().to_usize(),
                                    t.range.start().to_usize() + t.id.as_str().len(),
                                );
                                c.interface_isinstance(t.id.as_str(), span);
                            }
                        }
                    }
                }
            }

            let func_type = infer_expr(c, &call.func);
            let call_span = (call.range.start().to_usize(), call.range.end().to_usize());

            // Argument access check on the receiver (for things like x.foo()
            // where x could be None).
            if let Expr::Attribute(attr) = call.func.as_ref() {
                let recv = infer_expr(c, &attr.value);
                if recv.is_nullable() {
                    if let Expr::Name(n) = attr.value.as_ref() {
                        let span = (
                            n.range.start().to_usize(),
                            n.range.start().to_usize() + n.id.as_str().len(),
                        );
                        c.nullable_use(n.id.as_str(), &recv, span);
                    }
                }
            }

            match func_type {
                Type::Function {
                    params,
                    ret,
                    variadic,
                } => {
                    // Argument count check honours defaults, keyword args,
                    // `*args`, and `**kwargs` by looking up the function's
                    // rich `ArityInfo` (when known by name) — FINDINGS #44.
                    // For callable values whose origin we can't resolve
                    // (e.g. `apply(f, v)` where `f: Callable[[int], int]`),
                    // we fall back to the structural shape: positional
                    // count must match `params.len()` exactly unless
                    // `variadic`, and kwargs are accepted iff the
                    // structural type spelled `*args` / `**kwargs`.
                    let fn_name: Option<String> = match call.func.as_ref() {
                        Expr::Name(n) => Some(n.id.as_str().to_owned()),
                        _ => None,
                    };
                    let arity_info: Option<&ArityInfo> = fn_name
                        .as_deref()
                        .and_then(|n| c.function_arity_info.get(n));
                    let count_ok = if let Some(info) = arity_info {
                        check_arity_with_info(info, pos_args, kw_args)
                    } else if variadic {
                        pos_args.len() >= params.len()
                    } else {
                        pos_args.len() == params.len()
                    };
                    if !count_ok {
                        let name = fn_name.clone().unwrap_or_else(|| "<call>".to_owned());
                        c.wrong_args(&name, params.len(), pos_args.len(), call_span);
                    }
                    // Argument type checks (per-pair, ignoring excess).
                    // Each argument is inferred with the corresponding
                    // formal as its expected type so empty-collection
                    // literals (`[]`, `{}`) and nested generic calls pick
                    // up the parameter's element types.
                    let mut actuals: Vec<Type> = Vec::with_capacity(params.len());
                    for (i, arg) in pos_args.iter().enumerate() {
                        if i >= params.len() {
                            break;
                        }
                        let actual = infer_expr_ctx(c, arg, Some(&params[i]));
                        // Check the nullable-use case first: when the actual
                        // is nullable and the parameter is not, `nullable_use`
                        // is the more helpful diagnostic — it points at the
                        // narrowing fix (`if x is not None:` / `guard`).
                        // Emitting `type_mismatch` alongside would just be
                        // noise on the same span (FINDINGS #8). Only emit the
                        // type_mismatch when nullable_use isn't going to fire.
                        let nullable_into_non_nullable =
                            !params[i].is_nullable() && actual.is_nullable();
                        if nullable_into_non_nullable {
                            if let Expr::Name(n) = arg {
                                let span = (
                                    n.range.start().to_usize(),
                                    n.range.start().to_usize() + n.id.as_str().len(),
                                );
                                c.nullable_use(n.id.as_str(), &params[i], span);
                            } else {
                                // Non-name arg (e.g. `greet(find())`) — no
                                // identifier to point at, fall back to the
                                // generic mismatch diagnostic.
                                let span =
                                    (arg.range().start().to_usize(), arg.range().end().to_usize());
                                c.mismatch(&params[i], &actual, span);
                            }
                        } else if !c.is_assignable(&params[i], &actual) {
                            let span =
                                (arg.range().start().to_usize(), arg.range().end().to_usize());
                            c.mismatch(&params[i], &actual, span);
                        }
                        actuals.push(actual);
                    }
                    // PEP 695 inference: bind every TypeVar mentioned in
                    // the formals from the actuals, then substitute in the
                    // declared return type so callers see a concrete
                    // result instead of `Any`.
                    // Also check that each inferred binding satisfies the
                    // TypeVar's declared bound (e.g. `T: Interface`).
                    if let Expr::Name(fn_name_expr) = call.func.as_ref() {
                        c.check_call_typevar_bounds(
                            fn_name_expr.id.as_str(),
                            &params,
                            &actuals,
                            &ret,
                            expected,
                            call_span,
                        );
                    }
                    // Bidirectional pass: if any TypeVar in `ret` is still
                    // unbound after the forward arg pass, use the call's
                    // expected return type (from the enclosing annotation
                    // or `return` statement) to pin it.
                    bind_typevars_and_substitute_bidirectional(&params, &actuals, &ret, expected)
                }
                Type::Class(name) => Type::Class(name),
                Type::Unknown | Type::Any => {
                    for a in pos_args.iter() {
                        let _ = infer_expr(c, a);
                    }
                    Type::Unknown
                }
                other => {
                    c.not_callable(&other, call_span);
                    Type::Unknown
                }
            }
        }
        Expr::Attribute(a) => {
            let recv = infer_expr(c, &a.value);
            if recv.is_nullable() {
                if let Expr::Name(n) = a.value.as_ref() {
                    let span = (
                        n.range.start().to_usize(),
                        n.range.start().to_usize() + n.id.as_str().len(),
                    );
                    c.nullable_use(n.id.as_str(), &recv, span);
                }
            }
            let attr_name = a.attr.as_str();
            // Resolve attribute access on known class instances and TypeVar-bounded parameters.
            if let Some(method_type) = builtin_generic_method(&recv, attr_name) {
                return method_type;
            }
            match &recv {
                Type::Class(class_name) => {
                    let class_name = class_name.clone();
                    if let Some(sig) = c.find_method(class_name.as_str(), attr_name) {
                        let arity = sig.arity;
                        let ret = sig.return_type.clone();
                        return Type::Function {
                            params: vec![Type::Unknown; arity],
                            ret: Box::new(ret),
                            variadic: false,
                        };
                    }
                    if let Some(field_type) = c.find_field(class_name.as_str(), attr_name) {
                        return field_type.clone();
                    }
                    // Not found — class may have dynamic attrs; no error.
                    Type::Unknown
                }
                Type::TypeVar(tv_name) => {
                    // TypeVar with a declared bound — look up the attribute in
                    // the bound's class/interface hierarchy.
                    let tv_name = tv_name.clone();
                    let bound = c.active_typevar_bounds.get(tv_name.as_str()).cloned();
                    if let Some(Type::Class(bound_name)) = bound {
                        if let Some(sig) = c.find_method(bound_name.as_str(), attr_name) {
                            let arity = sig.arity;
                            let ret = sig.return_type.clone();
                            return Type::Function {
                                params: vec![Type::Unknown; arity],
                                ret: Box::new(ret),
                                variadic: false,
                            };
                        }
                        if let Some(field_type) = c.find_field(bound_name.as_str(), attr_name) {
                            return field_type.clone();
                        }
                        // Attribute not found in the bound's hierarchy — emit error
                        // labelled at the attribute name token, not the full expression.
                        let attr_start = a.attr.range.start().to_usize();
                        let attr_len = a
                            .attr
                            .range
                            .end()
                            .to_usize()
                            .saturating_sub(attr_start)
                            .max(1);
                        if c.unsafe_depth == 0 {
                            c.diagnostics.push_error(TycError::attribute_not_found(
                                attr_name,
                                bound_name.as_str(),
                                &c.path,
                                c.source,
                                attr_start,
                                attr_len,
                            ));
                        }
                    }
                    Type::Unknown
                }
                _ => Type::Unknown,
            }
        }
        Expr::Subscript(s) => {
            let _ = infer_expr(c, &s.value);
            let _ = infer_expr(c, &s.slice);
            Type::Unknown
        }
        Expr::List(l) => {
            // Empty list: borrow the expected element type when available so
            // `let xs: list[int] = []` produces `list[int]` rather than
            // `list[?]`.
            if l.elts.is_empty() {
                if let Some(Type::Generic(head, args)) = expected {
                    if head == "list" && args.len() == 1 {
                        return Type::Generic("list".into(), args.clone());
                    }
                }
                return Type::Generic("list".into(), vec![Type::Unknown]);
            }
            // Non-empty literal: infer the element type from the union of
            // the elements'. If we have an expected `list[E]`, propagate `E`
            // into each element so nested literals (`[[]]`) can converge.
            let elt_expected = match expected {
                Some(Type::Generic(h, a)) if h == "list" && a.len() == 1 => Some(&a[0]),
                _ => None,
            };
            let elts: Vec<Type> = l
                .elts
                .iter()
                .map(|e| infer_expr_ctx(c, e, elt_expected))
                .collect();
            // Heterogeneous-list widening (FINDINGS #45): when every
            // inferred element is assignable to the expected element type
            // (interface / sealed-union / nominal-subtype rules included),
            // narrow the list's element type to the expectation rather
            // than the joined union of the elements. Without this, a
            // literal `[Button(...), Slider(...)]` infers as
            // `list[Button | Slider]` and fails the invariant
            // assignability check against `list[Drawable]` even when
            // both classes structurally conform to `Drawable`.
            //
            // Skip widening when the expected element type is itself an
            // unbound TypeVar — otherwise PEP 695 inference would lose
            // the concrete element types it needs to bind `T`, and the
            // backward-pass from a let annotation could mask real
            // mismatches (cf. `bidirectional_self_binding_does_not_block_backward_pass`).
            if let Some(exp) = elt_expected {
                if !is_typevar(exp) && elts.iter().all(|t| c.is_assignable(exp, t)) {
                    return Type::Generic("list".into(), vec![exp.clone()]);
                }
            }
            Type::Generic("list".into(), vec![Type::union_of(elts)])
        }
        Expr::Tuple(t) => {
            // Fixed-length tuple: when the expected type is
            // `tuple[T1, T2, ...]` with the same arity, propagate each
            // slot's expected element type into the corresponding
            // literal element so nested empty literals / generic calls
            // pick up their type.
            let per_slot: Option<&Vec<Type>> = match expected {
                Some(Type::Generic(h, a)) if h == "tuple" && a.len() == t.elts.len() => Some(a),
                _ => None,
            };
            let elts: Vec<Type> = t
                .elts
                .iter()
                .enumerate()
                .map(|(i, e)| infer_expr_ctx(c, e, per_slot.map(|a| &a[i])))
                .collect();
            Type::Generic("tuple".into(), elts)
        }
        Expr::Dict(d) => {
            // Tease apart key/value expected types from `dict[K, V]`.
            let (key_expected, val_expected) = match expected {
                Some(Type::Generic(h, a)) if h == "dict" && a.len() == 2 => {
                    (Some(&a[0]), Some(&a[1]))
                }
                _ => (None, None),
            };
            if d.items.is_empty() {
                if let (Some(k), Some(v)) = (key_expected, val_expected) {
                    return Type::Generic("dict".into(), vec![k.clone(), v.clone()]);
                }
                return Type::Generic("dict".into(), vec![Type::Unknown, Type::Unknown]);
            }
            // The dict display can mix `key: value` pairs with `**mapping`
            // unpacks.  Ruff models the unpack as `key = None`, `value =
            // <mapping>`.  Treat the unpack's mapping as a contribution
            // of its own K/V so we don't get `vals = [int, dict[str, int]]`
            // for `{"a": 1, **d}`.
            let map_expected = expected.cloned();
            let mut keys = Vec::with_capacity(d.items.len());
            let mut vals = Vec::with_capacity(d.items.len());
            for item in &d.items {
                if let Some(k) = &item.key {
                    keys.push(infer_expr_ctx(c, k, key_expected));
                    vals.push(infer_expr_ctx(c, &item.value, val_expected));
                } else {
                    // `**mapping`: infer the mapping with the surrounding
                    // dict's expected type, then split its K/V.
                    let m = infer_expr_ctx(c, &item.value, map_expected.as_ref());
                    if let Type::Generic(name, args) = &m {
                        if name == "dict" && args.len() == 2 {
                            keys.push(args[0].clone());
                            vals.push(args[1].clone());
                            continue;
                        }
                    }
                    // Anything else (Any, Unknown, weird mapping type): fall
                    // back to Unknown for both slots so we don't fabricate
                    // a misleading union.
                    keys.push(Type::Unknown);
                    vals.push(Type::Unknown);
                }
            }
            // Heterogeneous-dict widening (FINDINGS #45): when each
            // value is assignable to the expected V (using the
            // checker's interface / sealed-union rules), narrow the
            // emitted dict's value type to the expectation rather than
            // joining the literal's distinct value types into a union.
            // Skip widening when V is an unbound TypeVar — see the
            // matching note on the list arm.
            if let Some(v_exp) = val_expected {
                if !is_typevar(v_exp) && vals.iter().all(|t| c.is_assignable(v_exp, t)) {
                    let widened_k = match key_expected {
                        Some(k) if keys.iter().all(|t| c.is_assignable(k, t)) => k.clone(),
                        _ if keys.is_empty() => Type::Unknown,
                        _ => Type::union_of(keys),
                    };
                    return Type::Generic("dict".into(), vec![widened_k, v_exp.clone()]);
                }
            }
            let key_ty = if keys.is_empty() {
                Type::Unknown
            } else {
                Type::union_of(keys)
            };
            Type::Generic("dict".into(), vec![key_ty, Type::union_of(vals)])
        }
        Expr::Set(s) => {
            if s.elts.is_empty() {
                if let Some(Type::Generic(head, args)) = expected {
                    if head == "set" && args.len() == 1 {
                        return Type::Generic("set".into(), args.clone());
                    }
                }
                return Type::Generic("set".into(), vec![Type::Unknown]);
            }
            let elt_expected = match expected {
                Some(Type::Generic(h, a)) if h == "set" && a.len() == 1 => Some(&a[0]),
                _ => None,
            };
            let elts: Vec<Type> = s
                .elts
                .iter()
                .map(|e| infer_expr_ctx(c, e, elt_expected))
                .collect();
            // Heterogeneous-set widening (FINDINGS #45) — same shape
            // as the list / dict cases above. Skip when the expected
            // element type is an unbound TypeVar.
            if let Some(exp) = elt_expected {
                if !is_typevar(exp) && elts.iter().all(|t| c.is_assignable(exp, t)) {
                    return Type::Generic("set".into(), vec![exp.clone()]);
                }
            }
            Type::Generic("set".into(), vec![Type::union_of(elts)])
        }
        _ => Type::Unknown,
    }
}

// ── sealed union helpers ──────────────────────────────────────────────────────

/// Extract the list of variant class names from a `type Foo = A | B | C`
/// value expression.  Returns `None` if the expression is not a pure union of
/// bare names (meaning it is not a sealed union declaration we can track).
///
/// Uses an explicit stack rather than recursion to avoid stack overflow on
/// deeply nested union expressions (e.g. `A | B | C | ... | Z`).
fn extract_sealed_union_variants(expr: &Expr) -> Option<Vec<String>> {
    let mut names = Vec::new();
    let mut stack = vec![expr];
    while let Some(current) = stack.pop() {
        match current {
            Expr::Name(n) => names.push(n.id.as_str().to_owned()),
            Expr::BinOp(b) if matches!(b.op, Operator::BitOr) => {
                stack.push(&b.left);
                stack.push(&b.right);
            }
            _ => return None,
        }
    }
    if names.len() >= 2 {
        Some(names)
    } else {
        None
    }
}

// ── match exhaustiveness ──────────────────────────────────────────────────────

/// Check that a `match` statement on a sealed union covers all variants or
/// has a wildcard arm.  Emits a [`TycError::NonExhaustiveMatch`] if not.
fn check_match_exhaustiveness(
    c: &mut Checker,
    cases: &[MatchCase],
    union_name: &str,
    variants: &[String],
    subject_span: (usize, usize),
) {
    let mut covered: HashSet<String> = HashSet::new();
    let mut has_wildcard = false;

    for case in cases {
        // A guarded arm is conditional: `case Circle() if cond:` does not
        // cover `Circle` when `cond` is false.  Skip guarded arms for both
        // wildcard detection and variant coverage.
        if case.guard.is_some() {
            continue;
        }
        if is_wildcard_pattern(&case.pattern) {
            has_wildcard = true;
            break;
        }
        collect_matched_class_names(&case.pattern, &mut covered);
    }

    if has_wildcard {
        return;
    }

    let missing: Vec<&str> = variants
        .iter()
        .filter(|v| !covered.contains(v.as_str()))
        .map(String::as_str)
        .collect();

    if !missing.is_empty() {
        let missing_str = missing.join(", ");
        c.non_exhaustive_match(union_name, &missing_str, subject_span);
    }
}

/// Return `true` if this pattern unconditionally matches any value (wildcard).
///
/// - `case _:` → `MatchAs { pattern: None, name: None }` — wildcard.
/// - `case x:` → `MatchAs { pattern: None, name: Some("x") }` — wildcard capture.
/// - `case <wild> as x:` → `MatchAs { pattern: Some(<wild>), ... }` — wildcard iff
///   the inner pattern is also a wildcard (recursive check).
fn is_wildcard_pattern(pattern: &Pattern) -> bool {
    match pattern {
        Pattern::MatchAs(a) => match &a.pattern {
            None => true,
            Some(inner) => is_wildcard_pattern(inner),
        },
        Pattern::MatchOr(o) => o.patterns.iter().any(is_wildcard_pattern),
        _ => false,
    }
}

/// Collect the class names matched by `PatternMatchClass` nodes in a pattern,
/// recursing through wrapper forms (`MatchAs`, `MatchOr`) so that patterns
/// like `case Circle() as c:` count as covering the `Circle` variant.
fn collect_matched_class_names(pattern: &Pattern, covered: &mut HashSet<String>) {
    match pattern {
        Pattern::MatchClass(mc) => {
            if let Expr::Name(n) = mc.cls.as_ref() {
                covered.insert(n.id.as_str().to_owned());
            }
        }
        // `case Circle() as c:` — unwrap the alias and count the inner class.
        Pattern::MatchAs(a) => {
            if let Some(inner) = &a.pattern {
                collect_matched_class_names(inner, covered);
            }
        }
        Pattern::MatchOr(o) => {
            for p in &o.patterns {
                collect_matched_class_names(p, covered);
            }
        }
        _ => {}
    }
}

/// Declare pattern-bound names in the current scope as `Type::Unknown`, so
/// that references to them inside the case body do not produce spurious
/// "unknown name" errors.  Spans are set to the enclosing pattern's range so
/// that any future narrowing diagnostics point at the right source location.
fn bind_pattern_names(c: &mut Checker, pattern: &Pattern) {
    match pattern {
        Pattern::MatchAs(a) => {
            if let Some(name) = &a.name {
                c.env.declare(TypeBinding {
                    name: name.as_str().to_owned(),
                    declared: Type::Unknown,
                    narrowed: Type::Unknown,
                    span: (a.range.start().to_usize(), a.range.end().to_usize()),
                });
            }
            if let Some(inner) = &a.pattern {
                bind_pattern_names(c, inner);
            }
        }
        Pattern::MatchStar(s) => {
            if let Some(name) = &s.name {
                c.env.declare(TypeBinding {
                    name: name.as_str().to_owned(),
                    declared: Type::Unknown,
                    narrowed: Type::Unknown,
                    span: (s.range.start().to_usize(), s.range.end().to_usize()),
                });
            }
        }
        Pattern::MatchMapping(m) => {
            if let Some(rest) = &m.rest {
                c.env.declare(TypeBinding {
                    name: rest.as_str().to_owned(),
                    declared: Type::Unknown,
                    narrowed: Type::Unknown,
                    span: (m.range.start().to_usize(), m.range.end().to_usize()),
                });
            }
            for p in &m.patterns {
                bind_pattern_names(c, p);
            }
        }
        Pattern::MatchClass(mc) => {
            // Ruff bundles the positional and keyword sub-patterns into a
            // single `PatternArguments` value on `arguments`.
            for p in &mc.arguments.patterns {
                bind_pattern_names(c, p);
            }
            for kw in &mc.arguments.keywords {
                bind_pattern_names(c, &kw.pattern);
            }
        }
        Pattern::MatchOr(o) => {
            if let Some(first) = o.patterns.first() {
                bind_pattern_names(c, first);
            }
        }
        Pattern::MatchSequence(seq) => {
            for p in &seq.patterns {
                bind_pattern_names(c, p);
            }
        }
        Pattern::MatchValue(_) | Pattern::MatchSingleton(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tyc_resolve::resolve_module;
    use tyc_syntax::preprocess::preprocess;

    fn check(src: &str) -> Diagnostics {
        let prep = preprocess(src);
        let module = tyc_syntax::parse_module(&prep.python_source)
            .unwrap()
            .into_syntax();
        let (resolved, _) = resolve_module("<test>".to_owned(), &prep.python_source, &module);
        check_module_with(
            "<test>",
            &prep.python_source,
            &resolved,
            &module,
            &prep.unsafe_lines,
            &prep.frozen_class_lines,
        )
    }

    #[test]
    fn accepts_matching_annotation() {
        let d = check("let x: int = 1\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn rejects_type_mismatch() {
        let d = check("let x: int = \"hello\"\n");
        assert!(d.has_errors());
        let msg = format!("{}", d.errors()[0]);
        assert!(msg.contains("expected `int`"), "got {}", msg);
    }

    // ── Variance ─────────────────────────────────────────────────────────

    #[test]
    fn list_is_invariant_no_implicit_int_to_float_widening() {
        // Mutable container — promoting `list[int]` to `list[float]` is
        // unsound because a write through the wider view (`xs.append(2.5)`)
        // would put a float into a list the original holder expects to
        // contain only ints. The invariance rule rejects this.
        let d = check("mut xs: list[float] = [1.0, 2.0]\nlet ys: list[int] = [1, 2]\nxs = ys\n");
        assert!(
            d.has_errors(),
            "list invariance must reject list[int] -> list[float]; got: {:?}",
            d.errors()
        );
    }

    #[test]
    fn list_of_same_type_still_assigns() {
        // Sanity check: invariance for `list` doesn't break the common
        // `list[T] -> list[T]` case.
        let d = check("mut xs: list[int] = [1, 2]\nlet ys: list[int] = [3, 4]\nxs = ys\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn sequence_is_covariant_accepts_subtype_widening() {
        // Read-only view — `Sequence[int]` flows into `Sequence[float]`
        // covariantly. Tests the symmetric improvement: tightening
        // mutable-container variance shouldn't accidentally tighten
        // read-only-view variance. We construct both bindings as
        // `Sequence[T]` directly so the test exercises variance on
        // matching heads without needing list <: Sequence nominal
        // subtyping (which Typhon doesn't model today).
        let d = check("def f(xs: Sequence[int]) -> Sequence[float]:\n    return xs\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn dict_is_invariant_in_both_positions() {
        // `dict` is mutable on both axes (key reassignment, value
        // replacement), so neither parameter is variant. The function
        // form sidesteps the empty-dict literal type-inference quirk
        // (which would resolve to `dict[Any, Any]` and gloss over the
        // variance we want to test).
        let d = check("def f(m: dict[str, int]) -> dict[str, float]:\n    return m\n");
        assert!(d.has_errors(), "dict value invariance should reject");
    }

    #[test]
    fn mapping_is_covariant_in_value_position() {
        // The read-only `Mapping` ABC is invariant in the key (keys are
        // hashed/compared exactly) and covariant in the value.
        let d = check("def f(m: Mapping[str, int]) -> Mapping[str, float]:\n    return m\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn mapping_is_invariant_in_key_position() {
        // Keys must match exactly even on the read-only `Mapping`:
        // anyone iterating keys expects the declared type, and key
        // hashing is observable through `__getitem__`.
        let d = check("def f(m: Mapping[int, str]) -> Mapping[float, str]:\n    return m\n");
        assert!(d.has_errors(), "Mapping key invariance should reject");
    }

    #[test]
    fn variance_lookup_table_matches_spec() {
        // Unit-level check on the registry itself: misclassifying a
        // mutable builtin would silently introduce a soundness hole.
        assert_eq!(generic_param_variance("list", 0), Variance::Invariant);
        assert_eq!(generic_param_variance("set", 0), Variance::Invariant);
        assert_eq!(generic_param_variance("dict", 0), Variance::Invariant);
        assert_eq!(generic_param_variance("dict", 1), Variance::Invariant);
        assert_eq!(generic_param_variance("Sequence", 0), Variance::Covariant);
        assert_eq!(generic_param_variance("Iterable", 0), Variance::Covariant);
        assert_eq!(generic_param_variance("Mapping", 0), Variance::Invariant);
        assert_eq!(generic_param_variance("Mapping", 1), Variance::Covariant);
        assert_eq!(
            generic_param_variance("Callable", 0),
            Variance::Contravariant
        );
        assert_eq!(generic_param_variance("Callable", 1), Variance::Covariant);
        // Unknown head defaults to invariant — safest for user generics.
        assert_eq!(generic_param_variance("MyBox", 0), Variance::Invariant);
    }

    #[test]
    fn reassign_type_mismatch_carries_decl_site_and_explains_mut() {
        // Regression test for the misleading-diagnostic case raised on
        // PR #48: `mut greeting: str = "..."` followed by
        // `greeting = 8` previously produced a bare "expected str,
        // found int" message that read like a compiler bug. The new
        // diagnostic names the binding and both types in the headline,
        // anchors short labels to the declaration and the offending
        // value, and explains `mut` semantics in the help line.
        let d = check("mut greeting: str = \"hi\"\ngreeting = 8\n");
        assert!(d.has_errors(), "{:?}", d.errors());
        let err = &d.errors()[0];
        // Variant identity — the new dedicated diagnostic, not the
        // generic `TypeMismatch`.
        assert!(
            matches!(err, TycError::TypeReassignMismatch { .. }),
            "expected TypeReassignMismatch, got: {err:?}"
        );
        // Headline must name the binding, the actual type, and the
        // declared type so the user has every relevant fact in the
        // first line of output — the labels are intentionally terse
        // and only carry the per-anchor disambiguator.
        let msg = format!("{}", err);
        assert!(
            msg.contains("greeting") && msg.contains("str") && msg.contains("`int`"),
            "headline should name the binding and both types; got: {msg}"
        );
        if let TycError::TypeReassignMismatch {
            name,
            expected,
            actual,
            ..
        } = err
        {
            assert_eq!(name, "greeting");
            assert_eq!(expected, "str");
            assert_eq!(actual, "int");
        }
    }

    #[test]
    fn accepts_int_into_float_target() {
        let d = check("let x: float = 1\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn rejects_none_in_non_nullable() {
        let d = check("let x: int = None\n");
        assert!(d.has_errors());
    }

    #[test]
    fn accepts_none_in_nullable() {
        let d = check("let x: int | None = None\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn optional_sugar_accepted() {
        let d = check("let x: int? = None\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn narrowing_is_not_none() {
        let src = "\
def f(x: int | None) -> int:
    if x is not None:
        return x
    return 0
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn nullable_use_without_guard_errors() {
        let src = "\
def f(x: int | None) -> int:
    return x
";
        let d = check(src);
        assert!(d.has_errors(), "expected nullable-use error");
    }

    #[test]
    fn isinstance_narrows() {
        let src = "\
def f(x: int | str) -> int:
    if isinstance(x, int):
        return x
    return 0
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn wrong_arg_count_errors() {
        let src = "\
def add(a: int, b: int) -> int:
    return a + b

let r: int = add(1)
";
        let d = check(src);
        assert!(d.has_errors());
        let msg = format!("{}", d.errors()[0]);
        assert!(msg.contains("wrong number of arguments"), "got {}", msg);
    }

    #[test]
    fn wrong_arg_type_errors() {
        let src = "\
def add(a: int, b: int) -> int:
    return a + b

let r: int = add(1, \"x\")
";
        let d = check(src);
        assert!(d.has_errors());
    }

    #[test]
    fn class_type_recognised() {
        let src = "\
class Point:
    x: int
    y: int

let p: Point = Point()
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn not_returns_bool_regardless_of_operand() {
        // `not x` on a non-bool operand still has type bool.
        let d = check("let flag: bool = not 1\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn typing_optional_attribute_resolves() {
        let src = "\
import typing

def f(x: typing.Optional[int]) -> int:
    if x is not None:
        return x
    return 0
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn typing_union_attribute_resolves() {
        let src = "\
import typing

def f(x: typing.Union[int, str]) -> int:
    if isinstance(x, int):
        return x
    return 0
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    // ── sealed union tests ────────────────────────────────────────────────────

    #[test]
    fn variant_assignable_to_sealed_union() {
        let src = "\
class Circle:
    radius: int

class Rectangle:
    width: int

type Shape = Circle | Rectangle

let s: Shape = Circle()
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn exhaustive_match_passes() {
        let src = "\
class Circle:
    radius: int

class Rectangle:
    width: int

type Shape = Circle | Rectangle

let s: Shape = Circle()

match s:
    case Circle():
        pass
    case Rectangle():
        pass
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn non_exhaustive_match_errors() {
        let src = "\
class Circle:
    radius: int

class Rectangle:
    width: int

class Triangle:
    base: int

type Shape = Circle | Rectangle | Triangle

let s: Shape = Circle()

match s:
    case Circle():
        pass
    case Rectangle():
        pass
";
        let d = check(src);
        assert!(d.has_errors(), "expected non-exhaustive-match error");
        let msg = format!("{}", d.errors()[0]);
        assert!(
            msg.contains("Triangle"),
            "error should name missing variant Triangle, got: {msg}"
        );
    }

    #[test]
    fn wildcard_arm_satisfies_exhaustiveness() {
        let src = "\
class Circle:
    radius: int

class Rectangle:
    width: int

class Triangle:
    base: int

type Shape = Circle | Rectangle | Triangle

let s: Shape = Circle()

match s:
    case Circle():
        pass
    case _:
        pass
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn named_capture_arm_satisfies_exhaustiveness() {
        let src = "\
class Circle:
    radius: int

class Rectangle:
    width: int

type Shape = Circle | Rectangle

let s: Shape = Circle()

match s:
    case Circle():
        pass
    case other:
        pass
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn non_sealed_match_not_checked() {
        // A `match` on a plain int is not subject to exhaustiveness rules.
        let src = "\
let x: int = 1

match x:
    case 1:
        pass
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn guarded_arm_does_not_satisfy_exhaustiveness() {
        // `case Circle() if cond:` can miss when cond is false — it must not
        // count as full coverage of Circle.
        let src = "\
class Circle:
    radius: int

class Rectangle:
    width: int

type Shape = Circle | Rectangle

let s: Shape = Circle()

match s:
    case Circle() if True:
        pass
    case Rectangle():
        pass
";
        let d = check(src);
        assert!(
            d.has_errors(),
            "guarded arm must not satisfy exhaustiveness"
        );
        let msg = format!("{}", d.errors()[0]);
        assert!(
            msg.contains("Circle"),
            "error should name Circle, got: {msg}"
        );
    }

    #[test]
    fn as_pattern_satisfies_exhaustiveness() {
        // `case Circle() as c:` must count as covering Circle.
        let src = "\
class Circle:
    radius: int

class Rectangle:
    width: int

type Shape = Circle | Rectangle

let s: Shape = Circle()

match s:
    case Circle() as c:
        pass
    case Rectangle():
        pass
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn variant_assignable_to_nullable_sealed_union() {
        // `val x: Shape? = Circle()` must be accepted: Circle is a variant of
        // Shape, and Shape? == Shape | None.
        let src = "\
class Circle:
    radius: int

class Rectangle:
    width: int

type Shape = Circle | Rectangle

let x: Shape? = Circle()
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    // ── Result[T, E] type tests ───────────────────────────────────────────────

    #[test]
    fn ok_assignable_to_result() {
        let src = "\
let r: Result[int, str] = Ok(42)
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn err_assignable_to_result() {
        let src = "\
let r: Result[int, str] = Err(\"oops\")
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn plain_int_not_assignable_to_result() {
        let src = "\
let r: Result[int, str] = 42
";
        let d = check(src);
        assert!(d.has_errors(), "expected type-mismatch error");
    }

    #[test]
    fn result_return_type_ok() {
        let src = "\
def find(id: int) -> Result[int, str]:
    return Ok(id)
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn result_return_type_err() {
        let src = "\
def find(id: int) -> Result[int, str]:
    return Err(\"not found\")
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn result_wrong_ok_type_rejected() {
        // Ok("text") should not fit Result[int, str]: Ok expects an int value.
        let src = "\
let r: Result[int, str] = Ok(\"text\")
";
        let d = check(src);
        assert!(
            d.has_errors(),
            "expected type-mismatch: Ok[str] not assignable to Result[int, str]"
        );
    }

    #[test]
    fn result_wrong_err_type_rejected() {
        // Err(99) should not fit Result[int, str]: Err expects a str value.
        let src = "\
let r: Result[int, str] = Err(99)
";
        let d = check(src);
        assert!(
            d.has_errors(),
            "expected type-mismatch: Err[int] not assignable to Result[int, str]"
        );
    }

    #[test]
    fn result_assignable_to_same_result() {
        // Regression: Result[T, E] → Result[T, E] must type-check. The old
        // logic returned false in the `_` arm of the constructor match,
        // breaking the structural Generic-Generic fallback.
        let src = "\
def first() -> Result[int, str]:
    return Ok(1)

def forward() -> Result[int, str]:
    let r: Result[int, str] = first()
    return r
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    // ── Interface conformance (Phase 3) ──────────────────────────────────────

    #[test]
    fn class_conforming_to_interface_passes() {
        let src = "\
interface Drawable:
    def draw(self) -> None:
        ...

class Circle:
    def draw(self) -> None:
        pass

let d: Drawable = Circle()
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn class_missing_interface_method_rejected() {
        let src = "\
interface Drawable:
    def draw(self) -> None:
        ...
    def area(self) -> float:
        ...

class Circle:
    def draw(self) -> None:
        pass

let s: Drawable = Circle()
";
        let d = check(src);
        assert!(d.has_errors(), "missing-member must fail conformance");
        let msg = format!("{}", d.errors()[0]);
        assert!(
            msg.contains("area"),
            "diagnostic should name missing member: {msg}"
        );
        assert!(
            msg.contains("Drawable"),
            "diagnostic should name interface: {msg}"
        );
    }

    #[test]
    fn isinstance_against_interface_rejected() {
        let src = "\
interface Drawable:
    def draw(self) -> None:
        ...

class Circle:
    def draw(self) -> None:
        pass

let c = Circle()
let ok: bool = isinstance(c, Drawable)
";
        let d = check(src);
        assert!(
            d.has_errors(),
            "isinstance(x, Interface) should be rejected"
        );
        let msg = format!("{}", d.errors()[0]);
        assert!(
            msg.contains("Drawable") || msg.contains("structural"),
            "expected interface-isinstance diagnostic, got: {msg}"
        );
    }

    #[test]
    fn isinstance_against_class_still_works() {
        // Non-interface classes are not affected by the interface-isinstance
        // rejection.
        let src = "\
class Circle:
    radius: float

let c = Circle()
let ok: bool = isinstance(c, Circle)
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn isinstance_against_runtime_checkable_interface_allowed() {
        // @runtime_checkable opts in to attribute-presence isinstance — the
        // user accepts the weaker guarantee. The checker should NOT reject
        // this form.
        let src = "\
@runtime_checkable
interface Drawable:
    def draw(self) -> None:
        ...

class Circle:
    def draw(self) -> None:
        pass

let c = Circle()
let ok: bool = isinstance(c, Drawable)
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "@runtime_checkable interface must permit isinstance: {:?}",
            d.errors()
        );
    }

    // ── PEP 695 type parameters (Phase 3) ────────────────────────────────────

    #[test]
    fn pep695_function_type_params_resolve() {
        let src = "\
def first[T](items: list[T]) -> T:
    return items[0]
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn pep695_generic_call_assignable_to_concrete_target() {
        // With T treated as Any in the signature, the return value satisfies
        // any annotation at the call site (until real inference lands).
        let src = "\
def first[T](items: list[T]) -> T:
    return items[0]

let xs: list[int] = [1, 2, 3]
let n: int = first(xs)
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn bind_typevars_substitutes_simple_return() {
        // Direct unit test of the substitution helper: given `f[T](T) -> T`,
        // applying actual `int` to formal `T` should make the return `int`.
        let formals = vec![Type::TypeVar("T".to_owned())];
        let actuals = vec![Type::Int];
        let ret = Type::TypeVar("T".to_owned());
        let inferred = bind_typevars_and_substitute(&formals, &actuals, &ret);
        assert_eq!(inferred, Type::Int);
    }

    #[test]
    fn bind_typevars_substitutes_inside_generic() {
        // `f[T](list[T]) -> T` with actual `list[str]` should infer `T = str`.
        let formals = vec![Type::Generic(
            "list".to_owned(),
            vec![Type::TypeVar("T".to_owned())],
        )];
        let actuals = vec![Type::Generic("list".to_owned(), vec![Type::Str])];
        let ret = Type::TypeVar("T".to_owned());
        let inferred = bind_typevars_and_substitute(&formals, &actuals, &ret);
        assert_eq!(inferred, Type::Str);
    }

    #[test]
    fn bind_typevars_conflicting_args_widens_to_union() {
        // Calling `f[T](T, T)` with `(int, str)` should bind `T = int | str`.
        let formals = vec![Type::TypeVar("T".to_owned()), Type::TypeVar("T".to_owned())];
        let actuals = vec![Type::Int, Type::Str];
        let ret = Type::TypeVar("T".to_owned());
        let inferred = bind_typevars_and_substitute(&formals, &actuals, &ret);
        // Union variants are unordered semantically; check membership.
        match inferred {
            Type::Union(variants) => {
                assert!(variants.contains(&Type::Int));
                assert!(variants.contains(&Type::Str));
            }
            other => panic!("expected union, got {other:?}"),
        }
    }

    #[test]
    fn bidirectional_expected_pins_unbound_return_typevar() {
        // `make[T]() -> list[T]` has no args to infer T from, but the
        // call-site annotation `list[int]` should pin T.
        let formals: Vec<Type> = vec![];
        let actuals: Vec<Type> = vec![];
        let ret = Type::Generic("list".to_owned(), vec![Type::TypeVar("T".to_owned())]);
        let expected = Type::Generic("list".to_owned(), vec![Type::Int]);
        let inferred =
            bind_typevars_and_substitute_bidirectional(&formals, &actuals, &ret, Some(&expected));
        assert_eq!(inferred, Type::Generic("list".to_owned(), vec![Type::Int]));
    }

    #[test]
    fn bidirectional_forward_pass_wins_over_expected() {
        // When args already bind T, the expected type must not override the
        // forward result (the args carry more authoritative information).
        let formals = vec![Type::TypeVar("T".to_owned())];
        let actuals = vec![Type::Int];
        let ret = Type::TypeVar("T".to_owned());
        // Caller expects str but argument is int — arg wins, so the return
        // stays int and the assignment-check downstream catches the mismatch.
        let inferred =
            bind_typevars_and_substitute_bidirectional(&formals, &actuals, &ret, Some(&Type::Str));
        assert_eq!(inferred, Type::Int);
    }

    #[test]
    fn bidirectional_forward_binding_not_widened_by_expected() {
        // Regression for the backward-pass widening bug: with forward T=int
        // and expected return str, T must stay `int` (not be widened to
        // `int | str`).  At the assignment site the resulting `int` then
        // fails to match the `str` annotation, surfacing the real error.
        let src = "\
def id[T](x: T) -> T:
    return x

let s: str = id(3)
";
        let d = check(src);
        assert!(
            d.has_errors(),
            "id(3) returns int; assigning to str must be rejected"
        );
        // The diagnostic must be a mismatch on `int`, not on `int | str`.
        let widened = d
            .errors()
            .iter()
            .any(|e| e.to_string().contains("int | str"));
        assert!(
            !widened,
            "expected type must not widen the forward-bound TypeVar; errors: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn bidirectional_tuple_propagates_expected_per_slot() {
        // `tuple[list[int], list[str]] = ([], [])` should propagate each
        // slot's element type into the corresponding empty list literal.
        let src = "\
let pair: tuple[list[int], list[str]] = ([], [])
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
        // Swapping the element types in the literal must now be caught.
        let bad = "\
let pair: tuple[list[int], list[str]] = ([\"x\"], [1])
";
        let d2 = check(bad);
        assert!(
            d2.has_errors(),
            "list[str] in slot 0 (expected list[int]) must be rejected"
        );
    }

    #[test]
    fn bidirectional_self_binding_does_not_block_backward_pass() {
        // `head[T](xs: list[T]) -> T` called with `[]`: the empty literal
        // is inferred under the formal `list[T]`, so the literal becomes
        // `list[T]` (carrying T forward).  Without the self-binding
        // suppression, the forward pass would record `T → T` and the
        // backward pass would be skipped, leaving the call's return as
        // the literal TypeVar T.  Verify that the annotation actually
        // drives inference by checking that an incompatible annotation is
        // rejected.
        let src = "\
def head[T](xs: list[T]) -> T:
    return xs[0]

let n: int = head([])
let s: str = head([])
let bad: int = head([\"x\"])
";
        let d = check(src);
        // The first two `let` lines must type-check (T is pinned by each
        // annotation respectively).  The third must fail: `[\"x\"]` is
        // `list[str]`, so T=str, return is str, not assignable to int.
        let errs: Vec<String> = d.errors().iter().map(|e| e.to_string()).collect();
        assert!(
            errs.iter().any(|m| m.contains("int")),
            "expected an int-vs-str mismatch on the bad assignment; errs: {errs:?}"
        );
    }

    #[test]
    fn bidirectional_pinned_typevar_still_bound_checked() {
        // `def mk[T: int]() -> T` has no args to bind T; the backward pass
        // would pin T=str from the `let s: str = mk()` annotation.  Bound
        // validation must still fire — `str` does not satisfy `T: int`.
        let src = "\
def mk[T: int]() -> T:
    return 0

let s: str = mk()
";
        let d = check(src);
        assert!(
            has_typevar_bound_error(&d),
            "T=str pinned by expected return must still violate the T: int bound; \
             errors: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn dict_unpack_merges_mapping_kv_not_value() {
        // `{"a": 1, **d}` where `d: dict[str, int]` must produce
        // `dict[str, int]`, not `dict[str, int | dict[str, int]]`.
        let src = "\
def take(m: dict[str, int]) -> None:
    pass

let d: dict[str, int] = {\"x\": 1}
take({\"a\": 2, **d})
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "dict unpack should merge K/V, not push the mapping into vals: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn bidirectional_empty_list_picks_up_annotation() {
        // `let xs: list[int] = []` should now type-check: the empty list
        // borrows the annotation's `int` element type.
        let src = "let xs: list[int] = []\n";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn bidirectional_empty_dict_picks_up_annotation() {
        let src = "let m: dict[str, int] = {}\n";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn list_literal_infers_element_union() {
        // `[1, "x"]` should infer `list[int | str]`, not `list[?]`.  Verify
        // by feeding it to a function whose parameter is `list[int]` and
        // checking the call is rejected.
        let src = "\
def take_ints(xs: list[int]) -> None:
    pass

take_ints([1, \"x\"])
";
        let d = check(src);
        assert!(
            d.has_errors(),
            "list[int|str] is not assignable to list[int]"
        );
    }

    #[test]
    fn list_literal_homogeneous_passes_strict_param() {
        let src = "\
def take_ints(xs: list[int]) -> None:
    pass

take_ints([1, 2, 3])
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn bidirectional_empty_arg_pins_typevar_via_param() {
        // `def head[T](xs: list[T]) -> T:` then `head([])` — without a
        // call-site annotation we have no information, so T stays unbound
        // (return ends up as the literal TypeVar T) and the call is
        // permitted (TypeVar acts as Any).  But when the parameter type
        // flows into the empty list, the literal becomes `list[T]`, and
        // bidirectional inference at the outer call should still succeed
        // when the result is assigned to a concrete type.
        let src = "\
def head[T](xs: list[T]) -> T:
    return xs[0]

let n: int = head([])
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn bidirectional_return_pins_typevar_for_factory() {
        // `make[T]() -> list[T]` has no args to drive inference; the
        // call-site annotation must do it.
        let src = "\
def make[T]() -> list[T]:
    return []

let xs: list[int] = make()
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn optional_param_against_concrete_arg_binds_inner() {
        // `def f[T](x: T | None) -> T` called with `int` should infer T=int,
        // not T = int|None — verified by assigning the return to `int`.
        let src = "\
def unwrap[T](x: T | None) -> T:
    if x is None:
        return x
    return x

let n: int = unwrap(3)
";
        // The body's narrowing isn't perfect, but the call-site inference
        // should still produce `int` for the return type.
        let d = check(src);
        // We only assert there's no assignment mismatch on `let n: int = unwrap(3)`.
        let has_mismatch = d.errors().iter().any(|e| {
            let msg = e.to_string();
            msg.contains("expected `int`") && msg.contains("unwrap")
        });
        assert!(
            !has_mismatch,
            "unwrap(3) should infer int, not int|None: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn pep695_inferred_return_flows_to_concrete_assignment() {
        // After inference, calling `first[T](list[T]) -> T` with `list[int]`
        // yields a return type of `int`, so assigning to a `str`-typed
        // binding must now be rejected.  Before inference this passed
        // because `T` was Any in the return position.
        let src = "\
def first[T](items: list[T]) -> T:
    return items[0]

let xs: list[int] = [1, 2, 3]
let n: str = first(xs)
";
        let d = check(src);
        assert!(
            d.has_errors(),
            "with PEP 695 inference, first(list[int]) returns int and must not assign to str"
        );
    }

    // ── TypeVar bound checking ────────────────────────────────────────────────

    #[test]
    fn typevar_bound_subclass_satisfies_parent_class_bound() {
        // `def f[T: Animal](x: T)` called with `Dog` — `Dog` inherits from
        // `Animal`, so it satisfies the `T: Animal` bound.  No diagnostic expected.
        let src = "\
class Animal:
    pass

class Dog(Animal):
    pass

def f[T: Animal](x: T) -> T:
    return x

let d: Dog = Dog()
let r: Dog = f(d)
";
        let d = check(src);
        assert!(
            !has_typevar_bound_error(&d),
            "Dog inherits from Animal so T: Animal should be satisfied; errors: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn typevar_bound_unrelated_class_emits_violation() {
        // `Cat` does not inherit from `Animal`, so passing it to `f[T: Animal]`
        // must still produce a diagnostic.
        let src = "\
class Animal:
    pass

class Cat:
    pass

def f[T: Animal](x: T) -> T:
    return x

let c: Cat = Cat()
let r: Cat = f(c)
";
        let d = check(src);
        assert!(
            has_typevar_bound_error(&d),
            "Cat does not inherit from Animal so T: Animal bound should be violated; errors: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn typevar_bound_interface_satisfied_by_conforming_class() {
        // `def f[T: Greeter](x: T)` called with `Dog` which conforms to `Greeter`
        // structurally — must be accepted without a typevar_bound diagnostic.
        let src = "\
interface Greeter:
    def greet(self) -> str: ...

class Dog:
    def greet(self) -> str:
        return \"woof\"

def f[T: Greeter](x: T) -> T:
    return x

let d: Dog = Dog()
let r: Dog = f(d)
";
        let d = check(src);
        assert!(
            !has_typevar_bound_error(&d),
            "Dog conforms to Greeter so T: Greeter bound should be satisfied; errors: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    fn has_typevar_bound_error(d: &Diagnostics) -> bool {
        // The TypeVarBoundViolation message template is:
        // "type argument `{actual}` for `{typevar}` does not satisfy bound `{bound}`"
        d.errors()
            .iter()
            .any(|e| e.to_string().contains("does not satisfy bound"))
    }

    #[test]
    fn typevar_bound_violated_emits_diagnostic() {
        // `def f[T: int](x: T) -> T` called with a `str` argument.
        // The bound says T must satisfy `int`; `str` is not assignable to
        // `int`, so a `tyc::typevar_bound` diagnostic must be emitted.
        let src = "\
def f[T: int](x: T) -> T:
    return x

let s: str = \"hello\"
let r: int = f(s)
";
        let d = check(src);
        assert!(
            has_typevar_bound_error(&d),
            "calling f[T: int] with str should emit a typevar_bound diagnostic; errors: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn typevar_no_bound_does_not_emit_bound_diagnostic() {
        // An unbounded typevar should never produce a typevar_bound error.
        let src = "\
def identity[T](x: T) -> T:
    return x

let s: str = \"hi\"
let r: str = identity(s)
";
        let d = check(src);
        assert!(
            !has_typevar_bound_error(&d),
            "unbounded typevar should not produce a typevar_bound diagnostic"
        );
    }

    // ── interface field type checking ────────────────────────────────────────

    #[test]
    fn interface_field_correct_type_passes() {
        let src = "\
interface Named:
    name: str

class Dog:
    name: str

let d: Named = Dog()
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "class with correct field type should conform to interface; errors: {:?}",
            d.errors()
        );
    }

    #[test]
    fn interface_field_wrong_type_rejected() {
        // `Dog.name: int` does not satisfy `Named.name: str`.
        let src = "\
interface Named:
    name: str

class Dog:
    name: int

let d: Named = Dog()
";
        let d = check(src);
        assert!(
            d.has_errors(),
            "class with wrong field type must not conform to interface"
        );
        let msg = format!("{}", d.errors()[0]);
        assert!(
            msg.contains("Named") || msg.contains("name"),
            "diagnostic should mention the interface or field; got: {msg}"
        );
    }

    #[test]
    fn interface_field_missing_rejected() {
        // `Dog` is missing the `name` field entirely.
        let src = "\
interface Named:
    name: str

class Dog:
    age: int

let d: Named = Dog()
";
        let d = check(src);
        assert!(
            d.has_errors(),
            "class missing required interface field must be rejected"
        );
        let msg = format!("{}", d.errors()[0]);
        assert!(
            msg.contains("name") || msg.contains("Named"),
            "diagnostic should name the missing field; got: {msg}"
        );
    }

    #[test]
    fn interface_field_mismatch_diagnostic_names_types() {
        // The diagnostic text should include both the expected and actual types.
        let src = "\
interface Config:
    port: int

class BadConfig:
    port: str

let c: Config = BadConfig()
";
        let d = check(src);
        assert!(d.has_errors(), "type-mismatched field must be rejected");
        let msg = format!("{}", d.errors()[0]);
        // The conformance error wraps field mismatches; the outer message names the interface.
        assert!(
            msg.contains("Config") || msg.contains("port"),
            "diagnostic should reference the interface or field name; got: {msg}"
        );
    }

    #[test]
    fn interface_field_accepts_conforming_class_for_interface_typed_field() {
        // When an interface field's type is itself an interface, a concrete class
        // that structurally conforms to that nested interface should be accepted.
        // This requires `self.is_assignable` (not the free `assignable`) so the
        // structural conformance check fires.
        let src = "\
interface Pet:
    name: str

interface Owner:
    pet: Pet

class Dog:
    name: str

class Person:
    pet: Dog

let p: Owner = Person()
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "concrete class satisfying nested interface field should conform; errors: {:?}",
            d.errors()
        );
    }

    #[test]
    fn interface_field_satisfied_by_zero_arity_method_passes() {
        // A zero-argument method (property-like) may satisfy an interface field.
        // In Typhon's class shape, `def name(self)` records arity 0 (self excluded).
        let src = "\
interface Named:
    name: str

class Dog:
    def name(self) -> str:
        return \"Fido\"

let d: Named = Dog()
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "zero-arity method should satisfy interface field; errors: {:?}",
            d.errors()
        );
    }

    #[test]
    fn interface_field_satisfied_by_nonzero_arity_method_rejected() {
        // A method with arguments must NOT satisfy an interface field requirement.
        let src = "\
interface Named:
    name: str

class Dog:
    def name(self, suffix: str) -> str:
        return \"Fido\" + suffix

let d: Named = Dog()
";
        let d = check(src);
        assert!(
            d.has_errors(),
            "method with arguments must not satisfy interface field; should be rejected"
        );
    }

    #[test]
    fn interface_method_return_type_mismatch_rejected() {
        let src = "\
interface Greeter:
    def greet(self) -> str: ...

class BadGreeter:
    def greet(self) -> int:
        return 42

let g: Greeter = BadGreeter()
";
        let d = check(src);
        assert!(
            d.has_errors(),
            "method with wrong return type should fail interface conformance; errors: {:?}",
            d.errors()
        );
    }

    #[test]
    fn interface_method_return_type_match_passes() {
        let src = "\
interface Greeter:
    def greet(self) -> str: ...

class GoodGreeter:
    def greet(self) -> str:
        return \"hello\"

let g: Greeter = GoodGreeter()
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "method with matching return type should pass interface conformance; errors: {:?}",
            d.errors()
        );
    }

    #[test]
    fn interface_method_unannotated_impl_now_rejected_by_rule_one() {
        // Rule 1 (every parameter and return type annotated) is now
        // enforced, so an unannotated `def greet(self):` is itself a
        // diagnostic before conformance is ever consulted. The conformance
        // path that treats `Type::Unknown` permissively still exists for
        // compiler-synthesised stubs; it just can't be reached through
        // user source without violating Rule 1.
        let src = "\
interface Greeter:
    def greet(self) -> str: ...

class BareGreeter:
    def greet(self):
        return \"hello\"

let g: Greeter = BareGreeter()
";
        let d = check(src);
        assert!(
            d.has_errors(),
            "Rule 1 should reject the unannotated `greet` impl; errors: {:?}",
            d.errors()
        );
        assert!(
            d.errors()
                .iter()
                .any(|e| format!("{e}").contains("missing a type annotation")),
            "expected a missing_annotation diagnostic; got: {:?}",
            d.errors()
        );
    }

    // ── Attribute resolution: class instances and TypeVar-bounded params ──────

    #[test]
    fn class_instance_method_call_type_checks() {
        let src = "\
class Greeter:
    def greet(self) -> str:
        return \"hello\"

let g: Greeter = Greeter()
let result: str = g.greet()
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "method call on class instance should type-check: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn typevar_bound_body_method_call_checks() {
        // Inside the function body, x: T where T: Greeter — x.greet() must be valid
        let src = "\
interface Greeter:
    def greet(self) -> str: ...

def call_greet[T: Greeter](x: T) -> str:
    return x.greet()
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "x.greet() on T: Greeter should type-check: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn typevar_bound_body_unknown_method_emits_error() {
        // x.nonexistent() on T: Greeter must emit attribute_not_found
        let src = "\
interface Greeter:
    def greet(self) -> str: ...

def call_bad[T: Greeter](x: T) -> str:
    return x.nonexistent()
";
        let d = check(src);
        assert!(
            d.has_errors(),
            "x.nonexistent() on T: Greeter should emit attribute_not_found"
        );
        assert!(
            d.errors()
                .iter()
                .any(|e| e.to_string().contains("nonexistent")),
            "error should mention the missing attribute"
        );
    }

    #[test]
    fn typevar_bound_body_field_access_works() {
        // x.name where T: Named (interface with field name: str) must work
        let src = "\
interface Named:
    name: str

def get_name[T: Named](x: T) -> str:
    return x.name
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "field access on T: Named should type-check: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    // ── frozen-class field-write rejection ───────────────────────────────────

    #[test]
    fn rejects_direct_write_to_frozen_field() {
        let src = "\
class Identity frozen:
    name: str

let i: Identity = Identity(name=\"Alice\")
i.name = \"Bob\"
";
        let d = check(src);
        assert!(
            d.has_errors(),
            "direct write to a frozen field must be rejected"
        );
        assert!(
            d.errors()
                .iter()
                .any(|e| e.to_string().contains("frozen") && e.to_string().contains("name")),
            "diagnostic should name the frozen field: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn rejects_nested_write_to_frozen_field() {
        // The exact case from the user's bug report: a frozen field is
        // reached through one or more attribute hops from a mutable
        // container. The receiver `outer.inner` resolves to a frozen
        // class, so the final write must be rejected.
        let src = "\
class Identity frozen:
    name: str

class User:
    identity: Identity
    age: int

let user: User = User(identity=Identity(name=\"Alice\"), age=30)
user.identity.name = \"Bob\"
";
        let d = check(src);
        assert!(
            d.has_errors(),
            "nested write to a frozen field must be rejected"
        );
    }

    #[test]
    fn rejects_aug_assign_to_frozen_field() {
        let src = "\
class Counter frozen:
    n: int

let c: Counter = Counter(n=0)
c.n += 1
";
        let d = check(src);
        assert!(
            d.has_errors(),
            "augmented assignment to a frozen field must be rejected"
        );
    }

    #[test]
    fn rejects_self_write_inside_impl_of_frozen_class() {
        // The user can still write `impl FrozenClass: def m(self): self.x = ...`;
        // we want the same diagnostic to fire there, not just at outside
        // call sites.
        let src = "\
class Identity frozen:
    name: str

impl Identity:
    def rename(self, new_name: str) -> None:
        self.name = new_name
";
        let d = check(src);
        assert!(
            d.has_errors(),
            "self-write inside impl of frozen class must be rejected"
        );
        // The diagnostic should display the original class name, not the
        // `__typhon_impl_*` pseudo-class name.
        assert!(
            d.errors()
                .iter()
                .any(|e| e.to_string().contains("`Identity`")),
            "diagnostic should display the user-visible class name `Identity`, not the pseudo: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn allows_write_to_mutable_class_field() {
        let src = "\
class User:
    name: str

impl User:
    def rename(self, new_name: str) -> None:
        self.name = new_name

let u: User = User(name=\"Alice\")
u.name = \"Bob\"
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "writes to mutable class fields must still pass: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn allows_field_declarations_in_frozen_class_body() {
        // `class X frozen:` field annotations (`name: str`) are
        // declarations, not assignments — they must not be flagged.
        let src = "\
class Identity frozen:
    name: str
    age: int = 0
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "field declarations inside a frozen class body must not be flagged: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }
}
