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

use ruff_python_ast::{Expr, MatchCase, ModModule, Number, Operator, Pattern, Stmt, StmtAssign};
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
    /// An imported Python module reference. Carries the dotted module
    /// name (e.g. `"foo.bar"`) so attribute access (`f.Cls(...)`) can
    /// look the target class up in a project-wide module shape
    /// registry. Bare `import M` / `import M as N` bindings land
    /// here; the seeded `from M import X` form continues to land
    /// directly as `Type::Class("X")` because the local name *is*
    /// the class. FINDINGS #163.
    Module(String),
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
                // `tuple_variadic[T]` is the internal name for the
                // homogeneous-variadic tuple type written `tuple[T, ...]`
                // in source. Render it back as the source form so
                // diagnostics quote what the user wrote.
                if name == "tuple_variadic" && args.len() == 1 {
                    return format!("tuple[{}, ...]", args[0].display());
                }
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
            Type::Module(name) => format!("<module {name}>"),
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
            // Generator[Y, S, R] / AsyncGenerator[Y, S] are structurally
            // assignable to Iterable[T] / Iterator[T] (sync) and
            // AsyncIterable[T] / AsyncIterator[T] (async) when the yielded
            // element type Y is assignable to the target element T
            // covariantly. Without this rule, a function that `yield`s
            // (inferred as Generator[...]) could never satisfy a
            // `-> Iterable[T]` return annotation, which is the common shape
            // users write. The recursive `assignable` call carries the
            // existing variance and union-flattening rules.
            if (an == "Iterable" || an == "Iterator")
                && bn == "Generator"
                && aa.len() == 1
                && !bb.is_empty()
            {
                return assignable(&aa[0], &bb[0]);
            }
            if (an == "AsyncIterable" || an == "AsyncIterator")
                && bn == "AsyncGenerator"
                && aa.len() == 1
                && !bb.is_empty()
            {
                return assignable(&aa[0], &bb[0]);
            }
            // `tuple[T, ...]` (homogeneous variadic) accepts any
            // fixed-length `tuple[T1, …, Tn]` whose elements are all
            // assignable to `T`, plus the empty tuple `tuple[]`. The
            // reverse direction (`tuple[T1, T2]` accepting a
            // `tuple_variadic`) is not allowed — the consumer asked for
            // a specific arity, the producer can't promise it.
            if an == "tuple_variadic" && aa.len() == 1 && bn == "tuple" {
                return bb.iter().all(|t| assignable(&aa[0], t));
            }
            // `tuple_variadic[T1]` assignable from `tuple_variadic[T2]`
            // covariantly — tuples are immutable so the read-only
            // direction is sound.
            if an == "tuple_variadic" && bn == "tuple_variadic" && aa.len() == 1 && bb.len() == 1 {
                return assignable(&aa[0], &bb[0]);
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
        // Function / Function — structural callable assignability.
        // `Callable[..., R]` (modelled as empty params + `variadic`)
        // accepts any function with an assignable return type. For
        // fixed-arity callables, expected/actual must share arity and
        // each parameter pair is contravariant (a callable that
        // tolerates a wider input type can stand in for one expecting
        // a narrower input).
        (
            Type::Function {
                params: ep,
                ret: er,
                variadic: ev,
            },
            Type::Function {
                params: ap,
                ret: ar,
                variadic: _av,
            },
        ) => {
            if *ev && ep.is_empty() {
                return assignable(er, ar);
            }
            if ep.len() != ap.len() {
                return false;
            }
            // Contravariant params, covariant return.
            ep.iter().zip(ap).all(|(e, a)| assignable(a, e)) && assignable(er, ar)
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

/// Return `true` when any statement in `body` (recursively) contains
/// a `yield` or `yield from` expression. Used by the return-type
/// check in `check_function` to flag generator-shaped function bodies
/// whose return annotation isn't iterator-shaped (FINDINGS #51).
///
/// Nested function and class bodies are skipped — a `yield` inside an
/// inner generator doesn't make the *outer* function a generator. We
/// dispatch through `visit_stmt` (not `walk_stmt`) at the top level
/// so the visitor's own pruning of `Stmt::FunctionDef` / `Stmt::ClassDef`
/// fires before we descend.
fn body_has_yield(body: &[Stmt]) -> bool {
    struct YieldVisitor<'a> {
        found: &'a mut bool,
    }
    impl<'a, 'b> ruff_python_ast::visitor::source_order::SourceOrderVisitor<'a> for YieldVisitor<'b> {
        fn visit_expr(&mut self, e: &'a Expr) {
            if *self.found {
                return;
            }
            if matches!(e, Expr::Yield(_) | Expr::YieldFrom(_)) {
                *self.found = true;
                return;
            }
            // Don't descend into nested function / lambda definitions.
            if matches!(e, Expr::Lambda(_)) {
                return;
            }
            ruff_python_ast::visitor::source_order::walk_expr(self, e);
        }
        fn visit_stmt(&mut self, s: &'a Stmt) {
            if *self.found {
                return;
            }
            if matches!(s, Stmt::FunctionDef(_) | Stmt::ClassDef(_)) {
                return;
            }
            ruff_python_ast::visitor::source_order::walk_stmt(self, s);
        }
    }
    let mut found = false;
    {
        let mut visitor = YieldVisitor { found: &mut found };
        for s in body {
            use ruff_python_ast::visitor::source_order::SourceOrderVisitor;
            visitor.visit_stmt(s);
            if *visitor.found {
                break;
            }
        }
    }
    found
}

/// Return `true` when `body` (recursively) contains an `await`
/// expression — including the implicit awaits inside `async for` and
/// Mandatory async-protocol dunders — these are legitimately allowed to
/// have an `async def` body with no `await` (e.g. `__aenter__` that
/// returns `self`, `__aexit__` that just records cleanup state, or an
/// `__anext__` that immediately raises `StopAsyncIteration`). Suppress
/// `tyc::async_without_await` for them. FINDINGS #114.
fn is_async_protocol_dunder(name: &str) -> bool {
    matches!(name, "__aenter__" | "__aexit__" | "__aiter__" | "__anext__")
}

/// Return `true` when `body` is structurally a declaration-only body —
/// any combination of `pass`, ellipsis expression statements, and
/// docstrings (string literal expression statements). These bodies are
/// typical of Protocol / interface method declarations and should not
/// trigger `tyc::async_without_await`.
fn body_is_declaration_only(body: &[Stmt]) -> bool {
    if body.is_empty() {
        return true;
    }
    body.iter().all(|s| match s {
        Stmt::Pass(_) => true,
        Stmt::Expr(e) => matches!(
            e.value.as_ref(),
            Expr::EllipsisLiteral(_) | Expr::StringLiteral(_)
        ),
        _ => false,
    })
}

/// `async with` headers. Nested function and class bodies are skipped
/// so an `await` in an inner async lambda or nested coroutine doesn't
/// satisfy the outer function. FINDINGS #83.
fn body_has_await(body: &[Stmt]) -> bool {
    struct AwaitVisitor<'a> {
        found: &'a mut bool,
    }
    impl<'a, 'b> ruff_python_ast::visitor::source_order::SourceOrderVisitor<'a> for AwaitVisitor<'b> {
        fn visit_expr(&mut self, e: &'a Expr) {
            if *self.found {
                return;
            }
            if matches!(e, Expr::Await(_)) {
                *self.found = true;
                return;
            }
            // Don't descend into nested function-like expressions.
            if matches!(e, Expr::Lambda(_)) {
                return;
            }
            ruff_python_ast::visitor::source_order::walk_expr(self, e);
        }
        fn visit_stmt(&mut self, s: &'a Stmt) {
            if *self.found {
                return;
            }
            // `async for` / `async with` headers (`Stmt::For` / `Stmt::With`
            // with `is_async = true`) count as `await` for the purposes of
            // this check.
            match s {
                Stmt::For(f) if f.is_async => {
                    *self.found = true;
                    return;
                }
                Stmt::With(w) if w.is_async => {
                    *self.found = true;
                    return;
                }
                Stmt::FunctionDef(_) | Stmt::ClassDef(_) => return,
                _ => {}
            }
            ruff_python_ast::visitor::source_order::walk_stmt(self, s);
        }
    }
    let mut found = false;
    {
        let mut visitor = AwaitVisitor { found: &mut found };
        for s in body {
            use ruff_python_ast::visitor::source_order::SourceOrderVisitor;
            visitor.visit_stmt(s);
            if *visitor.found {
                break;
            }
        }
    }
    found
}

/// Return `true` when `returns` is annotated as one of the
/// generator-compatible types. Recognises both PEP 484 (`Iterator[T]`,
/// `Generator[T, S, R]`, `Iterable[T]`, `AsyncIterator[T]`,
/// `AsyncGenerator[T, S]`) and the bare names (`Iterator`,
/// `Generator`, etc.) which may flow in via stub imports.
fn is_iterator_return_type(returns: &Expr, is_async: bool) -> bool {
    let sync_names = ["Iterator", "Iterable", "Generator"];
    let async_names = ["AsyncIterator", "AsyncIterable", "AsyncGenerator"];
    let head = match returns {
        Expr::Name(n) => Some(n.id.as_str()),
        Expr::Subscript(s) => match s.value.as_ref() {
            Expr::Name(n) => Some(n.id.as_str()),
            Expr::Attribute(a) => Some(a.attr.as_str()),
            _ => None,
        },
        Expr::Attribute(a) => Some(a.attr.as_str()),
        _ => None,
    };
    let Some(name) = head else { return false };
    // `async def f() -> Iterator[T]` containing `yield` produces an
    // *async* generator at runtime — Python only accepts the
    // `AsyncIterator` / `AsyncIterable` / `AsyncGenerator` names for
    // those. Accept only the matching family.
    if is_async {
        async_names.contains(&name)
    } else {
        sync_names.contains(&name)
    }
}

/// Return `true` when the type is an unbound PEP 695 type parameter.
/// Used by the container-literal widening rules to avoid prematurely
/// resolving a TypeVar to a concrete element type (which would block
/// PEP 695 inference from binding it from the actual arguments).
fn is_typevar(t: &Type) -> bool {
    matches!(t, Type::TypeVar(_))
}

/// Return `true` when this call targets one of the well-known
/// asyncio entry-points that accept coroutines as direct arguments.
/// Used by the `missing_await` check (FINDINGS #49) to suppress
/// false positives on the canonical `asyncio.run(coro())` pattern.
fn call_targets_coro_acceptor(call: &ruff_python_ast::ExprCall) -> bool {
    let Expr::Attribute(a) = call.func.as_ref() else {
        return false;
    };
    let method = a.attr.as_str();
    let is_coro_method = matches!(
        method,
        "run"
            | "create_task"
            | "ensure_future"
            | "gather"
            | "wait"
            | "wait_for"
            | "as_completed"
            | "spawn"
    );
    if !is_coro_method {
        return false;
    }
    // Accept exactly two shapes:
    //   1. `asyncio.<method>(coro())`
    //   2. `typhon_runtime.tasks.<method>(coro())`
    // (Plus aliased forms `<X>.asyncio.<method>(...)` are accepted too —
    // common when users `import asyncio as aio` and re-export — but a
    // bare `.tasks.<method>` whose receiver isn't `typhon_runtime` is
    // rejected so a user module called `mypkg.tasks.gather(...)`
    // doesn't silently suppress `tyc::missing_await`.)
    match a.value.as_ref() {
        // `asyncio.<method>(...)`
        Expr::Name(n) if n.id.as_str() == "asyncio" => true,
        // `typhon_runtime.tasks.<method>(...)`
        Expr::Attribute(inner)
            if inner.attr.as_str() == "tasks"
                && matches!(inner.value.as_ref(), Expr::Name(n) if n.id.as_str() == "typhon_runtime") =>
        {
            true
        }
        // `pkg.asyncio.<method>(...)` — reasonably an aliased asyncio.
        Expr::Attribute(inner) if inner.attr.as_str() == "asyncio" => true,
        _ => false,
    }
}

/// Walk a generic-class field-type / arg-type pair and bind any
/// TypeVar mentioned in the field type to the corresponding shape of
/// the argument type. Used by the constructor-call inference path
/// (FINDINGS #46) so `class Box[T]: value: T` learns `T = int` from
/// `Box(value=42)` without needing the caller to spell `Box[int](...)`.
fn bind_field_typevars(field_ty: &Type, arg_ty: &Type, out: &mut HashMap<String, Type>) {
    match (field_ty, arg_ty) {
        (Type::TypeVar(name), other) if !matches!(other, Type::TypeVar(_)) => {
            out.entry(name.clone()).or_insert_with(|| other.clone());
        }
        (Type::Generic(_, fas), Type::Generic(_, aas)) if fas.len() == aas.len() => {
            for (f, a) in fas.iter().zip(aas) {
                bind_field_typevars(f, a, out);
            }
        }
        (Type::Union(fs), Type::Union(as_)) if fs.len() == as_.len() => {
            for (f, a) in fs.iter().zip(as_) {
                bind_field_typevars(f, a, out);
            }
        }
        _ => {}
    }
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

/// Widen a `Literal[...]` element expression to its underlying type.
/// `Literal["a"]` → `str`, `Literal[42]` → `int`, `Literal[True]` → `bool`,
/// `Literal[b"x"]` → `bytes`, `Literal[None]` → `None`. Anything else
/// (an identifier, a nested expression) falls back to `Type::Unknown`.
/// FINDINGS #98.
fn literal_widened_type(expr: &Expr) -> Type {
    match expr {
        Expr::StringLiteral(_) => Type::Str,
        Expr::NumberLiteral(n) => match &n.value {
            ruff_python_ast::Number::Int(_) => Type::Int,
            ruff_python_ast::Number::Float(_) => Type::Float,
            ruff_python_ast::Number::Complex { .. } => Type::Unknown,
        },
        Expr::BooleanLiteral(_) => Type::Bool,
        Expr::BytesLiteral(_) => Type::Bytes,
        Expr::NoneLiteral(_) => Type::None,
        Expr::UnaryOp(u) if matches!(u.op, ruff_python_ast::UnaryOp::USub) => {
            literal_widened_type(&u.operand)
        }
        _ => Type::Unknown,
    }
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
            // Synthetic shadow-resistant alias for the runtime `Err`,
            // injected by `?` and `with`-chain lowerings. Treat as
            // `Class("Err")` for type-checking purposes so post-`?`
            // narrowing and `result_error_mismatch` continue to work
            // even when the user shadowed `Err` with a type alias.
            // FINDINGS #104.
            "__typhon_Err__" => Type::Class("Err".into()),
            // `typing.Self` — represents "the current class". Without a
            // surrounding class context we treat it permissively as
            // `Type::Any` so builder-pattern methods that return
            // `Self` don't trip `tyc::type_mismatch` against the
            // implementation class. FINDINGS #97.
            "Self" => Type::Any,
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
            // Final[T], ClassVar[T] — both are transparent wrappers
            // that don't affect runtime types or assignability rules.
            // FINDINGS #99.
            if head == "Final" || head == "ClassVar" {
                return type_from_annotation_with_params(&s.slice, classes, type_params);
            }
            // Annotated[T, ...] — first slice arg is the real type;
            // the rest are runtime metadata that doesn't affect
            // assignability. FINDINGS #100.
            if head == "Annotated" {
                let real = match s.slice.as_ref() {
                    Expr::Tuple(t) if !t.elts.is_empty() => &t.elts[0],
                    other => other,
                };
                return type_from_annotation_with_params(real, classes, type_params);
            }
            // Literal["a", "b"] / Literal[1, 2] — narrow to the
            // widened literal type (str / int / bool / bytes / None).
            // FINDINGS #98.
            if head == "Literal" {
                let variants: Vec<&Expr> = match s.slice.as_ref() {
                    Expr::Tuple(t) => t.elts.iter().collect(),
                    other => vec![other],
                };
                let widened: Vec<Type> = variants.into_iter().map(literal_widened_type).collect();
                if widened.is_empty() {
                    return Type::Unknown;
                }
                return Type::union_of(widened);
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
                            // `Callable[..., R]` — any args (including
                            // zero), fixed return. Empty `params` plus
                            // `variadic: true` lets the call-site arity
                            // check accept 0..N positional arguments;
                            // using `vec![Type::Any]` would force at
                            // least one arg via the `total >=
                            // params.len()` path.
                            Expr::EllipsisLiteral(_) => {
                                return Type::Function {
                                    params: vec![],
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
                return Type::Generic("Callable".into(), vec![Type::Unknown, Type::Unknown]);
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
            //
            // Special case: `tuple[T, ...]` is the homogeneous-variadic
            // tuple type — same element type at every position, length
            // unconstrained. The trailing `...` is not a fixed slot, so
            // we collapse it to a one-argument `tuple_variadic[T]`
            // internal head. The assignability rules then accept any
            // fixed-length `tuple[T1, …, Tn]` whose elements are all
            // assignable to `T`. Display renders it back as
            // `tuple[T, ...]`. Without this carve-out the unifier would
            // see `tuple[T, ?]` (length 2) versus a 3-tuple literal
            // (length 3) and reject the assignment.
            if head == "tuple" {
                if let Expr::Tuple(t) = s.slice.as_ref() {
                    if t.elts.len() == 2 && matches!(t.elts[1], Expr::EllipsisLiteral(_)) {
                        let elem =
                            type_from_annotation_with_params(&t.elts[0], classes, type_params);
                        return Type::Generic("tuple_variadic".into(), vec![elem]);
                    }
                }
            }
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
    /// Names of `async def` functions declared at module top level.
    /// Used by the call-site arm to emit `tyc::missing_await`
    /// (FINDINGS #49) when a sync context calls one without `await`.
    async_functions: std::collections::HashSet<String>,
    /// Bumped on entry to an `Expr::Await`, decremented on exit. While
    /// positive, the call-site arm skips the `missing_await` check so
    /// the user's `await f()` is accepted.
    inside_await: u32,
    /// True while we are checking a *sync* function body. Only sync
    /// callers trip `tyc::missing_await`; `async def` bodies that
    /// forget to await are flagged separately by `async_without_await`
    /// (warn-level, not yet wired). Module scope is also exempt so
    /// the canonical `asyncio.run(coro())` entry-point pattern passes.
    in_sync_function: bool,
    /// True while we are checking the body of an `async def`. Used by
    /// the Phase-E blocking-in-async check (`tyc::blocking_in_async`)
    /// to fire on direct calls to known-blocking stdlib functions
    /// (`time.sleep`, `requests.get`, …) that should be wrapped in
    /// `await asyncio.to_thread(...)` instead.
    in_async_function: bool,
    /// True while we are checking the body of a generator function
    /// (any `def f() -> Iterator[T]` / `Generator[Y, S, R]` whose body
    /// contains `yield` / `yield from`). Inside a generator, `return`
    /// is `raise StopIteration(...)` — the value (if any) becomes the
    /// generator's `Return[R]` payload, *not* an `Iterator[T]`. The
    /// return-statement validator skips its usual assignability check
    /// while this flag is on, so that
    /// `def f() -> Iterator[int]: ... return` is accepted instead of
    /// being flagged as `expected Iterator[int], found None`.
    in_generator: bool,
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
    /// Transparent type-alias declarations: name → (type-parameter names,
    /// RHS type). Populated from every `type X = ...` statement, including
    /// the sealed-union ones (so an alias to `A | B | C` is *both* a sealed
    /// union and a transparent alias). Consulted by [`unwrap_alias`] before
    /// every assignability check so that `type Report = ReportData` allows
    /// `Ok(ReportData(...))` to satisfy `Result[Report, str]`, and so that
    /// `type B = int | str` accepts an `int` literal where `B` is required.
    /// FINDINGS #57, #58, #70.
    type_aliases: HashMap<String, (Vec<String>, Type)>,
    /// Nominal newtype declarations: name → base type. Populated from
    /// `newtype Name = Base` (preprocessed to `Name = NewType("Name",
    /// Base)`). Unlike `type_aliases`, newtypes are **asymmetric**:
    /// a `Name` flows freely into a `Base`-typed slot (escape upward),
    /// but a bare `Base` requires explicit construction via `Name(x)`
    /// before it satisfies a `Name`-typed target. The construction call
    /// itself type-checks the argument against `Base`.
    newtypes: HashMap<String, Type>,
    /// Interfaces (Typhon `interface Name:` → `class Name(Protocol):`).
    /// Maps the interface name to its required member shape and whether it
    /// opted in to runtime checking via `@runtime_checkable`. In v1 we
    /// check member presence only; full signature compatibility is deferred.
    interfaces: HashMap<String, InterfaceDecl>,
    /// All classes declared in the module along with their declared member
    /// names.  Used for structural conformance against an interface.
    class_shapes: HashMap<String, InterfaceShape>,
    /// PEP 695 type-parameter names declared on each generic class.
    /// `class Box[T]: ...` populates `{"Box": ["T"]}`. Empty for
    /// non-generic classes. Used to drive bidirectional inference at
    /// constructor calls so `let b: Box[int] = Box(value=42)` produces
    /// `Type::Generic("Box", [Int])` rather than `Type::Class("Box")`
    /// (FINDINGS #46).
    class_type_params: HashMap<String, Vec<String>>,
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
    /// Bindings constructed via the `X.__new__(X)` / `object.__new__(X)`
    /// bypass form, mapped to the class name and the set of required
    /// fields not yet assigned. When the binding flows into an escape
    /// position (return, function call argument) with a non-empty
    /// missing set, the audit emits `tyc::missing_field_init`. Cleared
    /// when the binding is reassigned to anything else, when
    /// `setattr(c, ...)` is called on it (dynamic assignment defeats
    /// the static tracker), or when a method on the binding is called
    /// (e.g. `c.configure(...)` which might assign fields). Skipped
    /// entirely inside `unsafe:` regions.
    uninit_instances: HashMap<String, UninitInstance>,
    /// Dotted-name keyed registry of every project module's shapes,
    /// used for attribute access on `Type::Module(name)` bindings. A
    /// bare `import foo as f` plus `f.ApiClient(...)` looks `f` up in
    /// the env → `Type::Module("foo")` → consults this registry for
    /// `foo`'s `ApiClient` class, returning a `Type::Class("ApiClient")`
    /// the constructor-call site can arity-check.
    module_registry: std::sync::Arc<HashMap<String, ModuleShapes>>,
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
pub struct ArityInfo {
    /// Names of the positional / pos-or-kw / kw-only parameters declared
    /// on the function, in source order. Used to match keyword arguments
    /// at call sites (`f(name="x")`).
    pub param_names: Vec<String>,
    /// Minimum number of positional arguments the caller must supply
    /// (i.e. count of params without default values, excluding kw-only).
    /// `def f(a, b=10) -> ...` → `min_positional = 1`.
    ///
    /// For free functions this equals the count of leading non-default
    /// params (Python enforces "no required after default"). For
    /// synthesised class constructors with `[strictness]
    /// model-required-anywhere` semantics (Pydantic-style), the
    /// per-param `required_positional` vector below is the source of
    /// truth — `min_positional` stays for callers that haven't
    /// migrated.
    pub min_positional: usize,
    /// Per-positional-parameter required flag, parallel to
    /// `param_names`. `true` means the caller must fill this slot
    /// either positionally or by matching kwarg; `false` means a
    /// default value covers it. For `def f(a, b=10)` this is
    /// `[true, false]`. For a Pydantic-style synthesised constructor
    /// like `model User: id: int = 1; name: str`, this is
    /// `[false, true]` — `name` is required even though it follows a
    /// defaulted field. Allows `check_arity_with_info` to honour the
    /// full "non-defaulted is required" rule regardless of parameter
    /// ordering. FINDINGS — codex review of v0.2.0.
    pub required_positional: Vec<bool>,
    /// Maximum number of positional arguments — the total count of
    /// posonlyargs + args. Kw-only params don't count. `None` for
    /// `*args` functions, which accept unbounded positionals.
    pub max_positional: Option<usize>,
    /// Names of kw-only parameters (after `*` or `*args`).
    pub kwonly_names: Vec<String>,
    /// Kw-only names that don't have a default value.
    pub kwonly_required: Vec<String>,
    /// True when the function declares `**kwargs`, accepting any
    /// otherwise-unmatched keyword argument.
    pub has_kwarg: bool,
    /// Declared element type of the `*args` variadic parameter, when
    /// the function has one. Used at call sites to type-check the
    /// excess positional args (FINDINGS #86). `Type::Unknown` when
    /// the vararg is unannotated.
    pub vararg_type: Option<Type>,
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
pub struct MethodSig {
    pub arity: usize,
    pub return_type: Type,
    /// `true` when the method was decorated with `@property`. Attribute
    /// access (`r.area`) on a property unwraps to `return_type` instead
    /// of producing a `() -> return_type` bound-method handle.
    /// FINDINGS #63.
    pub is_property: bool,
    /// `true` when the method was decorated with `@staticmethod`. Static
    /// methods have no implicit receiver, so `arity` records every listed
    /// parameter and the class-qualified call path does not add one for
    /// `self` (FINDINGS R3.16).
    pub is_static: bool,
    /// `true` when the method was decorated with `@classmethod`. Class
    /// methods take `cls` as the first parameter, but Python binds it
    /// automatically at every call site (instance- or class-qualified),
    /// so `arity` excludes `cls` and the class-qualified call path does
    /// not add an implicit receiver.
    pub is_classmethod: bool,
    /// Full arity metadata (param names, defaults, `*args`/`**kwargs`)
    /// for the method, mirroring [`ArityInfo`] for free functions. Used
    /// by call-site checking so `user.greet()` is flagged when `greet`
    /// has a required parameter, instead of falling through to the
    /// permissive "unknown callable" arity rule. Excludes the implicit
    /// receiver (`self` / `cls`) for instance / classmethods.
    pub arity_info: ArityInfo,
    /// Declared parameter types in source order, excluding the implicit
    /// receiver (`self` / `cls`) for instance / classmethods. Used at
    /// call sites to enforce per-arg type checks against the method's
    /// real signature — without this, methods fall back to a
    /// `vec![Type::Unknown; arity]` shape and the call site's
    /// nullable-into-non-nullable guard misfires on `T?` parameters
    /// (FINDINGS E2 / round 2026-05-20-exploration).
    pub param_types: Vec<Type>,
}

/// Member shape recorded for an interface or class — methods are recorded as
/// their parameter count (excluding `self`/`cls`), fields as their declared
/// type.
#[derive(Debug, Clone, Default)]
pub struct InterfaceShape {
    /// Method name → arity + return type.
    pub methods: HashMap<String, MethodSig>,
    /// Field name → annotation type.
    pub fields: HashMap<String, Type>,
    /// Field names in declaration order. The emitted `@dataclass` /
    /// `BaseModel` constructor binds positional arguments to fields in
    /// this order, so positional-arity matching and keyword-arg
    /// resolution at call sites consult this list rather than the
    /// HashMap (whose iteration order is unstable).
    pub field_order: Vec<String>,
    /// Fields with an explicit `= default` value in source. Used at
    /// constructor-call sites to decide whether the field is required:
    /// a field absent from this set must be filled by a positional arg
    /// or matching kwarg. Mirrors how `ArityInfo::min_positional` is
    /// derived for free functions. A nullable type (`T?`) alone does
    /// NOT add the field here — Typhon does not auto-inject `= None`
    /// for `T?` fields, so the runtime dataclass still requires them.
    pub field_defaults: std::collections::HashSet<String>,
}

/// Tracking state for a binding constructed via `X.__new__(X)` or
/// `object.__new__(X)`. The class name identifies which required-
/// field set we audit against; `missing` is the running set of fields
/// not yet assigned. When this binding escapes (return / call arg)
/// and `missing` is non-empty, the audit emits
/// `tyc::missing_field_init`. Bindings rebound to something else are
/// removed from the tracker so subsequent uses are unaffected.
#[derive(Debug, Clone)]
struct UninitInstance {
    /// The class being constructed (the argument to `__new__`).
    class: String,
    /// Required field names that haven't yet been seen on the
    /// left-hand side of an `<instance>.<field> = ...` assignment.
    missing: std::collections::HashSet<String>,
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
            async_functions: std::collections::HashSet::new(),
            inside_await: 0,
            in_sync_function: false,
            in_async_function: false,
            in_generator: false,
            function_type_bounds: HashMap::new(),
            active_typevar_bounds: HashMap::new(),
            interfaces: HashMap::new(),
            class_shapes: HashMap::new(),
            class_type_params: HashMap::new(),
            frozen_classes: std::collections::HashSet::new(),
            class_parents: HashMap::new(),
            unsafe_depth: 0,
            unsafe_line_starts: Vec::new(),
            sealed_unions: HashMap::new(),
            type_aliases: HashMap::new(),
            newtypes: HashMap::new(),
            env: TypeEnv::default(),
            diagnostics: Diagnostics::new(),
            current_return: None,
            current_class: None,
            uninit_instances: HashMap::new(),
            module_registry: std::sync::Arc::new(HashMap::new()),
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
        // Nominal newtype: `Email → str` is allowed (escape upward),
        // `Email → Email` is allowed (same name), but `str → Email` is
        // not — the caller must construct via `Email(x)`. The "escape
        // upward" rule lives here so a `UserId` flows freely into an
        // `int`-typed slot; the rejection direction is the default (we
        // fall through to `false`).
        if let Type::Class(expected_name) = expected {
            if let Type::Class(actual_name) = actual {
                if expected_name == actual_name {
                    return true;
                }
            }
            // Expected is not a newtype, but actual might be — unwrap and
            // retry. `Email → str` reaches this branch with `expected=str`
            // (a primitive) and `actual=Class("Email")`.
            if let Type::Class(actual_name) = actual {
                if let Some(base) = self.newtypes.get(actual_name.as_str()) {
                    if self.is_assignable(expected, base) {
                        return true;
                    }
                }
            }
        }
        if let Type::Class(actual_name) = actual {
            if let Some(base) = self.newtypes.get(actual_name.as_str()) {
                if self.is_assignable(expected, base) {
                    return true;
                }
            }
        }
        // Transparent type-alias unwrap. `type Report = ReportData` and
        // `type B = int | str` should let `Class("ReportData")` flow into
        // a target typed `Report`, and `Int` flow into `B`. `unwrap_alias`
        // returns the input untouched for non-alias types, so we recurse
        // only when at least one side actually changed (avoids infinite
        // loops on the no-op case). FINDINGS #57, #58, #70.
        let exp_unwrapped = self.unwrap_alias(expected);
        let act_unwrapped = self.unwrap_alias(actual);
        if &exp_unwrapped != expected || &act_unwrapped != actual {
            if assignable(&exp_unwrapped, &act_unwrapped) {
                return true;
            }
            if self.is_assignable(&exp_unwrapped, &act_unwrapped) {
                return true;
            }
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
            // Variadic-tuple coercion mirrors the rule in the free
            // `assignable` but recurses through `is_assignable` so
            // alias / sealed-union / interface conformance works for
            // the element type.
            if an == "tuple_variadic" && aa.len() == 1 && bn == "tuple" {
                return bb.iter().all(|t| self.is_assignable(&aa[0], t));
            }
            if an == "tuple_variadic" && bn == "tuple_variadic" && aa.len() == 1 && bb.len() == 1 {
                return self.is_assignable(&aa[0], &bb[0]);
            }
            if an == bn && aa.len() == bb.len() {
                return aa
                    .iter()
                    .zip(bb)
                    .enumerate()
                    .all(
                        |(idx, (formal, actual_arg))| match generic_param_variance(an, idx) {
                            Variance::Covariant => self.is_assignable(formal, actual_arg),
                            Variance::Contravariant => self.is_assignable(actual_arg, formal),
                            Variance::Invariant => {
                                self.is_assignable(formal, actual_arg)
                                    && self.is_assignable(actual_arg, formal)
                            }
                        },
                    );
            }
        }
        false
    }

    /// If `ty` (or its head, for a generic application) names a transparent
    /// type alias, return the alias's RHS with the alias's parameters
    /// substituted by the application's actual arguments. Returns the
    /// input unchanged for non-alias types or after a substitution failure.
    ///
    /// Handles chains (`type A = B; type B = int`) by recursing up to a
    /// fixed depth — both for performance and to break cycles introduced
    /// by `type A = B; type B = A` (FINDINGS #81 — circular aliases no
    /// longer cause infinite loops here; a dedicated diagnostic is left
    /// as follow-up).
    fn unwrap_alias(&self, ty: &Type) -> Type {
        self.unwrap_alias_inner(ty, 0)
    }

    fn unwrap_alias_inner(&self, ty: &Type, depth: u8) -> Type {
        // Bound the chain to prevent infinite loops on circular aliases.
        // Eight levels is far more than any realistic alias-of-alias chain.
        if depth >= 8 {
            return ty.clone();
        }
        match ty {
            Type::Class(name) => {
                if let Some((_params, rhs)) = self.type_aliases.get(name.as_str()) {
                    return self.unwrap_alias_inner(rhs, depth + 1);
                }
                ty.clone()
            }
            Type::Generic(name, args) => {
                if let Some((params, rhs)) = self.type_aliases.get(name.as_str()) {
                    if params.len() == args.len() {
                        let bindings: std::collections::HashMap<String, Type> =
                            params.iter().cloned().zip(args.iter().cloned()).collect();
                        let substituted = substitute_typevars(rhs, &bindings);
                        return self.unwrap_alias_inner(&substituted, depth + 1);
                    }
                }
                ty.clone()
            }
            _ => ty.clone(),
        }
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

    fn operator_type_mismatch(&mut self, op: &str, lhs: &Type, rhs: &Type, span: (usize, usize)) {
        if self.unsafe_depth > 0 {
            return;
        }
        let length = span.1.saturating_sub(span.0).max(1);
        self.diagnostics
            .push_error(TycError::operator_type_mismatch(
                op,
                lhs.display(),
                rhs.display(),
                &self.path,
                self.source,
                span.0,
                length,
            ));
    }

    fn tuple_index_out_of_range(&mut self, arity: usize, index: i64, span: (usize, usize)) {
        if self.unsafe_depth > 0 {
            return;
        }
        let length = span.1.saturating_sub(span.0).max(1);
        self.diagnostics
            .push_error(TycError::tuple_index_out_of_range(
                arity,
                index,
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

    /// Emit `tyc::missing_argument` when we can identify *which*
    /// parameters weren't filled. Preferred over [`Self::wrong_args`]
    /// for the "you forgot the required `client` kwarg" case — the
    /// caller sees the name they need to add instead of a count
    /// that buries the actionable detail. `missing` is the list of
    /// missing parameter names in declaration order; callers that
    /// can't enumerate the missing names (e.g. too many positionals)
    /// should keep using `wrong_args`.
    fn missing_argument(&mut self, name: &str, missing: Vec<String>, span: (usize, usize)) {
        if self.unsafe_depth > 0 {
            return;
        }
        if missing.is_empty() {
            return;
        }
        let length = span.1.saturating_sub(span.0).max(1);
        self.diagnostics.push_error(TycError::missing_argument(
            name,
            missing,
            &self.path,
            self.source,
            span.0,
            length,
        ));
    }

    /// Emit `tyc::unknown_kwarg` (FINDINGS #80). `suggestion` is the
    /// fully-formatted help string — either "did you mean `<x>`?" or a
    /// list of accepted parameter names.
    fn unknown_kwarg(
        &mut self,
        fn_name: &str,
        kwarg: &str,
        suggestion: String,
        span: (usize, usize),
    ) {
        if self.unsafe_depth > 0 {
            return;
        }
        let length = span.1.saturating_sub(span.0).max(1);
        self.diagnostics.push_error(TycError::unknown_kwarg(
            fn_name,
            kwarg,
            suggestion,
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

    fn missing_await(&mut self, callee: &str, span: (usize, usize)) {
        if self.unsafe_depth > 0 {
            return;
        }
        let length = span.1.saturating_sub(span.0).max(1);
        self.diagnostics.push_error(TycError::missing_await(
            callee,
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

/// Snapshot of a module's exported class shapes and free-function
/// arities. Built by [`extract_module_shapes`] and consumed by
/// [`check_module_with_imports`] so a checker walking module B can
/// reason about a class declared in module A. Stored behind an [`Arc`]
/// in the Salsa cache so cross-file invalidation stays cheap.
#[derive(Debug, Clone, Default)]
pub struct ModuleShapes {
    /// Every class declared at module top level, keyed by its declared
    /// name. Includes the merged `impl ClassName:` / `extend
    /// ClassName:` contributions, mirroring how the in-module checker
    /// sees them.
    pub class_shapes: HashMap<String, InterfaceShape>,
    /// Per-class PEP 695 type parameters, for generic-class constructor
    /// inference at cross-module call sites (`Box(value=42)` produces
    /// `Box[int]` even when `Box` is imported).
    pub class_type_params: HashMap<String, Vec<String>>,
    /// Free-function arities keyed by name. Forwarded to the checker
    /// so `from foo import bar; bar(1)` triggers the same
    /// `tyc::arg_count` check that an in-module `def bar` would.
    pub function_arities: HashMap<String, ArityInfo>,
}

/// Imports resolved to their source modules' [`ModuleShapes`], keyed
/// by the *local* name the import binds in the consumer module. The
/// CLI / LSP populates this before invoking [`check_module_with_imports`]
/// by walking each `import` statement in the resolved module, mapping
/// the dotted module path to a `ModuleShapes` snapshot, then keying
/// every brought-in symbol under its local alias.
///
/// Two forms are wired:
///
/// - `from M import X` (with or without `as Y`) → the local name `X`
///   (or `Y`) gets the class shape / function arity that module `M`
///   exports for `X`. Class shapes also land in `class_shapes` so
///   constructor arity checking fires immediately.
/// - `import M` / `import M as N` → the local name `M` (or `N`) is
///   bound to `Type::Module(M)`; attribute access (`M.SomeClass(...)`)
///   looks the target up in `by_module`. The dotted module name
///   stored on `Type::Module` is the *original* import path, not the
///   local alias, so the same module imported under different aliases
///   resolves consistently.
#[derive(Debug, Clone, Default)]
pub struct ExternalShapes {
    pub class_shapes: HashMap<String, InterfaceShape>,
    pub class_type_params: HashMap<String, Vec<String>>,
    pub function_arities: HashMap<String, ArityInfo>,
    /// Bare imports that need attribute-access resolution. Keyed by
    /// the *local* binding name; the value is the dotted module path
    /// the import refers to (e.g. `("np", "numpy")` for
    /// `import numpy as np`). Looked up by `seed_env_from_scope` to
    /// give the binding `Type::Module(...)`.
    pub bare_imports: HashMap<String, String>,
    /// Dotted-name keyed registry of every project module's shapes,
    /// for `Type::Module(name)` attribute access. Cloned into the
    /// checker so attribute access on a module-typed binding can find
    /// the foreign class shape without re-walking imports.
    /// Wrapped in [`Arc`] so cross-module callers can share the
    /// registry between many per-file `ExternalShapes` snapshots
    /// without paying an O(modules) clone per file. FINDINGS —
    /// copilot review of v0.2.0.
    pub by_module: std::sync::Arc<HashMap<String, ModuleShapes>>,
}

/// Light-weight first-pass extractor that walks a parsed module and
/// returns its exported class / function shapes. Reuses the same logic
/// as the type checker's `collect_classes_and_functions` first pass so
/// cross-module callers see the same field order, defaults, and arity
/// metadata that the in-module checker uses.
///
/// Safe to call before resolution / type-checking — the result depends
/// only on the AST structure, not on the resolver's scope tree. Errors
/// (cyclic aliases, unknown class names in annotations, …) are
/// silently tolerated: the goal here is to publish the surface API for
/// downstream callers, not to validate it.
pub fn extract_module_shapes(module: &ModModule) -> ModuleShapes {
    let mut classes: Vec<String> = Vec::new();
    for stmt in &module.body {
        match stmt {
            Stmt::ClassDef(cd) => classes.push(cd.name.as_str().to_owned()),
            Stmt::TypeAlias(ta) => {
                if let Expr::Name(n) = ta.name.as_ref() {
                    classes.push(n.id.as_str().to_owned());
                }
            }
            _ => {}
        }
    }

    let mut class_shapes: HashMap<String, InterfaceShape> = HashMap::new();
    let mut class_type_params: HashMap<String, Vec<String>> = HashMap::new();
    // First sweep: every declared class gets its own shape, including
    // the synthetic `__typhon_impl_NAME` pseudo-classes the
    // preprocessor introduces for `impl Name:` / `extend Name:`.
    for stmt in &module.body {
        if let Stmt::ClassDef(cd) = stmt {
            let name = cd.name.as_str().to_owned();
            let shape = collect_class_shape(cd, &classes);
            class_shapes.insert(name.clone(), shape);
            let tps = type_param_names_from(cd.type_params.as_deref());
            if !tps.is_empty() {
                class_type_params.insert(name, tps);
            }
        }
    }
    // Second sweep: fold `__typhon_impl_NAME` contributions back into
    // the target class so an out-of-module caller sees `impl`-block
    // methods on the same shape as the in-module checker does.
    for stmt in &module.body {
        if let Stmt::ClassDef(cd) = stmt {
            let pseudo = cd.name.as_str();
            if let Some(target) = pseudo.strip_prefix("__typhon_impl_") {
                if class_shapes.contains_key(target) {
                    let impl_shape = collect_class_shape(cd, &classes);
                    let target_shape = class_shapes.get_mut(target).expect("checked above");
                    for (m, sig) in impl_shape.methods {
                        target_shape.methods.entry(m).or_insert(sig);
                    }
                    // Move `field_defaults` out before the consuming
                    // `fields` iteration so we can re-key it under the
                    // same names we're inserting. Without this merge,
                    // an `impl X: y: int = 1` field would be (wrongly)
                    // treated as required at construction.
                    // FINDINGS — copilot review of v0.2.0.
                    let impl_defaults = impl_shape.field_defaults;
                    for (f, ty) in impl_shape.fields {
                        let is_new = !target_shape.fields.contains_key(&f);
                        if is_new {
                            target_shape.field_order.push(f.clone());
                            if impl_defaults.contains(&f) {
                                target_shape.field_defaults.insert(f.clone());
                            }
                        }
                        target_shape.fields.entry(f).or_insert(ty);
                    }
                }
            }
        }
    }
    // Drop the synthetic pseudo-classes from the published surface —
    // consumers should never see them by name.
    class_shapes.retain(|name, _| !name.starts_with("__typhon_impl_"));

    let mut function_arities: HashMap<String, ArityInfo> = HashMap::new();
    for stmt in &module.body {
        if let Stmt::FunctionDef(f) = stmt {
            let tps = type_param_names_from(f.type_params.as_deref());
            function_arities.insert(
                f.name.as_str().to_owned(),
                arity_info_from_parameters(f.parameters.as_ref(), &classes, &tps),
            );
        }
    }

    ModuleShapes {
        class_shapes,
        class_type_params,
        function_arities,
    }
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
    check_module_with_imports(
        path,
        source,
        resolved,
        module,
        unsafe_lines,
        frozen_class_lines,
        None,
    )
}

/// Variant of [`check_module_with`] that consults a pre-resolved
/// [`ExternalShapes`] snapshot so constructor + method arity checks
/// fire across module boundaries. The caller (CLI build / check, LSP
/// backend) is responsible for walking each import in the resolver's
/// binding table, looking up the source module's [`ModuleShapes`],
/// and re-keying every brought-in symbol under its local alias in the
/// returned `ExternalShapes`.
pub fn check_module_with_imports(
    path: impl Into<String>,
    source: &str,
    resolved: &ResolvedModule,
    module: &ModModule,
    unsafe_lines: &[usize],
    frozen_class_lines: &[usize],
    external: Option<&ExternalShapes>,
) -> Diagnostics {
    let mut c = Checker::new(path.into(), source, resolved);
    c.unsafe_line_starts = unsafe_byte_starts(source, unsafe_lines);
    let frozen_starts = unsafe_byte_starts(source, frozen_class_lines);
    // Seed cross-module shapes BEFORE the in-module first pass so
    // local declarations win on name collisions (a `class Foo` in
    // this file shadows an imported `Foo` for the rest of the
    // module body, mirroring Python's scope semantics).
    if let Some(ext) = external {
        for (name, shape) in &ext.class_shapes {
            c.class_shapes
                .entry(name.clone())
                .or_insert_with(|| shape.clone());
            c.classes.push(name.clone());
        }
        for (name, tps) in &ext.class_type_params {
            c.class_type_params
                .entry(name.clone())
                .or_insert_with(|| tps.clone());
        }
        for (name, info) in &ext.function_arities {
            c.function_arity_info
                .entry(name.clone())
                .or_insert_with(|| info.clone());
        }
        // Stash the by-module registry for attribute access on
        // `Type::Module(name)` (bare `import M` form). The clone
        // here is just an `Arc::clone` (O(1) refcount bump) thanks
        // to the wrapper in `ExternalShapes::by_module`, so this
        // doesn't pay an O(modules) cost per file.
        c.module_registry = std::sync::Arc::clone(&ext.by_module);
        // Bare imports: `bare_imports[local_name] = dotted_module`.
        // Seed `class_shapes` is NOT done here — the binding will
        // resolve via `Type::Module(...)` at attribute-access time,
        // not by name shadowing. The local name itself lands in the
        // env via `seed_env_from_scope` reading the resolver's
        // bindings; the type comes from the lookup below.
    }

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

    // Phase C: resource discipline. Walk the body for bound
    // `Stmt::Assign` / `Stmt::AnnAssign` whose RHS is a known
    // context-manager-returning call (`open`, `socket.socket`, …)
    // that wasn't consumed by a `with` statement. Fires
    // `tyc::resource_not_managed` as a warning; the strictness
    // filter promotes/demotes/drops based on `[strictness]
    // require-with` in `typhon.toml`.
    check_resource_discipline(&mut c, &module.body);

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
/// If `value` is a constructor-bypass call (`Cls.__new__(Cls)` or
/// `object.__new__(Cls)`), return the name of the class being
/// instantiated. The check requires the call to have exactly one
/// positional arg matching the receiver class, so the more dynamic
/// `cls.__new__(cls)` form (where `cls` is a `type` parameter) and
/// the `__new__(SomeBaseClass)` cross-class form are correctly NOT
/// detected — they need different audit semantics than the
/// "instance of X with X's fields" assumption we make below.
fn detect_new_bypass(value: &Expr) -> Option<String> {
    let call = match value {
        Expr::Call(c) => c,
        _ => return None,
    };
    let attr = match call.func.as_ref() {
        Expr::Attribute(a) => a,
        _ => return None,
    };
    if attr.attr.as_str() != "__new__" {
        return None;
    }
    let receiver = match attr.value.as_ref() {
        Expr::Name(n) => n.id.as_str(),
        _ => return None,
    };
    if call.arguments.args.len() != 1 || !call.arguments.keywords.is_empty() {
        return None;
    }
    let arg = match &call.arguments.args[0] {
        Expr::Name(n) => n.id.as_str(),
        _ => return None,
    };
    // Two shapes accepted:
    //   X.__new__(X)        — receiver == arg, the class itself
    //   object.__new__(X)   — receiver "object", arg the class
    if receiver == arg || receiver == "object" {
        Some(arg.to_owned())
    } else {
        None
    }
}

/// Mark `binding` as a bypass-constructed instance of `class_name`.
/// Pulls the required-field set from the class's [`InterfaceShape`]
/// (fields in `field_order` minus the ones in `field_defaults`).
/// Skipped inside `unsafe:` blocks where the user has opted out of
/// the static type discipline.
fn audit_register_bypass(c: &mut Checker, binding: &str, class_name: &str) {
    if c.unsafe_depth > 0 {
        return;
    }
    let Some(shape) = c.class_shapes.get(class_name) else {
        return;
    };
    let required: std::collections::HashSet<String> = shape
        .field_order
        .iter()
        .filter(|f| !shape.field_defaults.contains(*f))
        .cloned()
        .collect();
    if required.is_empty() {
        return;
    }
    c.uninit_instances.insert(
        binding.to_owned(),
        UninitInstance {
            class: class_name.to_owned(),
            missing: required,
        },
    );
}

/// Mark a field as initialised on a tracked bypass-constructed
/// instance. Called from the attribute-assignment site. No-op when
/// the LHS receiver isn't tracked.
fn audit_record_field_set(c: &mut Checker, target: &Expr) {
    let Expr::Attribute(attr) = target else {
        return;
    };
    let Expr::Name(recv) = attr.value.as_ref() else {
        return;
    };
    let recv_name = recv.id.as_str().to_owned();
    let field = attr.attr.as_str().to_owned();
    if let Some(entry) = c.uninit_instances.get_mut(&recv_name) {
        entry.missing.remove(&field);
    }
}

/// Drop a binding from the bypass tracker. Used when:
/// - the binding is reassigned to something other than another
///   bypass call (e.g. `c = SomeClass(...)`),
/// - dynamic attribute-setting calls like `setattr(c, ...)` are
///   detected (the static analysis can't follow them),
/// - a method is called on the instance (it might assign fields,
///   conservatively assume it does so),
/// - the binding goes out of scope (handled by `audit_clear_after_block`).
fn audit_clear_binding(c: &mut Checker, binding: &str) {
    c.uninit_instances.remove(binding);
}

/// Walk a call to detect side-effecting forms that should clear
/// bypass tracking on a passed-in binding:
/// - `setattr(c, "field", value)` — defeats static field tracking
/// - `c.method(...)` — method may assign fields internally
///
/// For the second case, the audit only flags an *escape* (return or
/// foreign call), so calling a method on the binding is still treated
/// as conservative: we drop the binding to avoid spurious diagnostics
/// after a likely-initialising helper. Erring on the side of
/// false-negatives matches the agent's design recommendation.
fn audit_observe_call(c: &mut Checker, call: &ruff_python_ast::ExprCall) {
    // setattr(c, ...) — drop c from tracking. The target binding may
    // be the first positional argument OR the `obj=` keyword (the
    // builtin's parameter name). Without the kwarg form, a call like
    // `setattr(obj=c, name="x", value=1)` would still trigger
    // false-positive `missing_field_init` diagnostics downstream.
    // FINDINGS — gemini review of v0.2.0.
    if let Expr::Name(fn_name) = call.func.as_ref() {
        if fn_name.id.as_str() == "setattr" {
            let obj_arg = call.arguments.args.first().or_else(|| {
                call.arguments
                    .keywords
                    .iter()
                    .find(|kw| {
                        kw.arg
                            .as_ref()
                            .map(|a| a.as_str() == "obj")
                            .unwrap_or(false)
                    })
                    .map(|kw| &kw.value)
            });
            if let Some(Expr::Name(n)) = obj_arg {
                audit_clear_binding(c, n.id.as_str());
            }
        }
    }
    // c.method(...) — drop c.
    if let Expr::Attribute(attr) = call.func.as_ref() {
        if let Expr::Name(recv) = attr.value.as_ref() {
            // Don't drop on the `X.__new__(X)` form — the receiver
            // there is the class, not a tracked binding, but better
            // safe than sorry.
            if attr.attr.as_str() != "__new__" {
                audit_clear_binding(c, recv.id.as_str());
            }
        }
    }
}

/// Emit `tyc::missing_field_init` for any tracked bindings appearing
/// in the given expression that still have a non-empty missing set.
/// Used at every escape point (return statement, function call
/// argument). Walks the expression to find `Name` nodes that match a
/// tracked binding, so `(c)` and `c` both fire but
/// `f(g(c))` is also caught.
fn audit_check_escape(c: &mut Checker, expr: &Expr) {
    if c.unsafe_depth > 0 {
        return;
    }
    let names = collect_names_in_expr(expr);
    // Take a snapshot up front so the diagnostic emit (which holds
    // `&mut c.diagnostics`) doesn't fight an outstanding immutable
    // borrow of `c.uninit_instances`.
    let snapshot: Vec<(String, UninitInstance)> = names
        .iter()
        .filter_map(|n| {
            c.uninit_instances
                .get(n)
                .filter(|info| !info.missing.is_empty())
                .map(|info| (n.clone(), info.clone()))
        })
        .collect();
    for (binding, info) in snapshot {
        let mut missing: Vec<String> = info.missing.into_iter().collect();
        missing.sort();
        let first = missing.first().cloned().unwrap_or_default();
        let missing_str = missing.join(", ");
        let span = (
            expr.range().start().to_usize(),
            expr.range().end().to_usize(),
        );
        let length = span.1.saturating_sub(span.0).max(1);
        c.diagnostics.push_error(TycError::missing_field_init(
            info.class,
            missing_str,
            first,
            &c.path,
            c.source,
            span.0,
            length,
        ));
        // Clear so the same instance doesn't fire again at the next
        // escape — the first diagnostic is enough to communicate
        // the bug.
        c.uninit_instances.remove(&binding);
    }
}

/// Collect the set of `Name` ids occurring in an expression that
/// represent the *instance itself escaping* (as opposed to having a
/// field read off them). `return c` yields `{"c"}`; `return c.field`
/// yields `{}` because only the field value flows out, not the
/// instance. `f(c)` yields `{"c"}`; `f(c.field)` yields `{}`.
///
/// The set is deduplicated so the same binding can't fire two
/// diagnostics for one escape site (e.g. `return c if cond else c`).
///
/// FINDINGS — gemini + copilot review of v0.2.0.
fn collect_names_in_expr(expr: &Expr) -> std::collections::HashSet<String> {
    use ruff_python_ast::visitor::source_order::{walk_expr, SourceOrderVisitor};
    struct V {
        names: std::collections::HashSet<String>,
    }
    impl<'a> SourceOrderVisitor<'a> for V {
        fn visit_expr(&mut self, e: &'a Expr) {
            match e {
                Expr::Name(n) => {
                    self.names.insert(n.id.as_str().to_owned());
                }
                // Skip the receiver of `recv.attr` — `c.field` reads
                // off `c`, it doesn't escape `c` itself. The other
                // sub-expressions (e.g. arguments to `c.attr` if it
                // were called — but that's an `Expr::Call` parent
                // that walks the receiver separately for the audit)
                // still get visited via `walk_expr` so nested
                // captures aren't missed.
                Expr::Attribute(a) => {
                    // Only short-circuit when the receiver is a
                    // bare name. For deeper chains like
                    // `f(c).field`, the inner `Expr::Call` still
                    // visits `c` (via its own arguments). Treating
                    // any `recv.attr` as non-escape would wrongly
                    // suppress audit on `(c if cond else d).field`,
                    // so we walk subexpressions when the receiver
                    // isn't a bare Name.
                    if matches!(a.value.as_ref(), Expr::Name(_)) {
                        // Skip — receiver-of-attribute access is a
                        // field read, not an instance escape.
                    } else {
                        walk_expr(self, &a.value);
                    }
                }
                _ => walk_expr(self, e),
            }
        }
    }
    let mut v = V {
        names: std::collections::HashSet::new(),
    };
    v.visit_expr(expr);
    v.names
}

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

/// Stdlib calls that block the event loop when invoked from inside an
/// `async def` body. Matched against the dotted callee path returned
/// by [`dotted_callee_path`], so `time.sleep`, `socket.recv`, and a
/// bare `input` all flow through the same lookup. Direct calls fire
/// `tyc::blocking_in_async`; the user can wrap them in `await
/// asyncio.to_thread(...)` (which the wrapper-detection below
/// excludes) or `loop.run_in_executor(...)` to silence the
/// diagnostic. Conservative — only the most common offenders.
const BLOCKING_CALLEES: &[&str] = &[
    // time
    "time.sleep",
    // I/O
    "input",
    // requests
    "requests.get",
    "requests.post",
    "requests.put",
    "requests.delete",
    "requests.patch",
    "requests.head",
    "requests.options",
    "requests.request",
    // urllib
    "urllib.request.urlopen",
    // socket (blocking sync ops)
    "socket.recv",
    "socket.send",
    "socket.recvfrom",
    "socket.sendto",
    "socket.accept",
    "socket.connect",
    // subprocess
    "subprocess.run",
    "subprocess.call",
    "subprocess.check_call",
    "subprocess.check_output",
];

/// The curated set of stdlib calls that return an unmanaged resource
/// (a file handle, socket, connection, …) which **must** be wrapped in
/// a `with` statement to guarantee cleanup. Matched as either a bare
/// name (`open(...)`) or a dotted suffix (`socket.socket(...)`,
/// `tempfile.NamedTemporaryFile(...)`). Conservative: only the entries
/// where missing-`with` is a real bug make the cut. Project-specific
/// classes can opt in via a future `@must_with` decorator or `.dty`
/// annotation.
const REQUIRE_WITH_CALLEES: &[&str] = &[
    "open",
    "socket.socket",
    "sqlite3.connect",
    "tempfile.NamedTemporaryFile",
    "tempfile.TemporaryDirectory",
    "tempfile.TemporaryFile",
];

/// Return the dotted callee path of `expr` if it is a `Call` whose
/// callee is a bare or dotted name; otherwise `None`. Used to match
/// against [`REQUIRE_WITH_CALLEES`] without false positives on
/// arbitrary expressions.
fn dotted_callee_path(expr: &Expr) -> Option<String> {
    let call = match expr {
        Expr::Call(c) => c,
        _ => return None,
    };
    dotted_name_of(&call.func)
}

fn dotted_name_of(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name(n) => Some(n.id.as_str().to_owned()),
        Expr::Attribute(a) => {
            let prefix = dotted_name_of(a.value.as_ref())?;
            Some(format!("{}.{}", prefix, a.attr.as_str()))
        }
        _ => None,
    }
}

/// Walk `body` recursively, firing `tyc::resource_not_managed` for any
/// assignment whose RHS is a known resource-returning call. A
/// `with`-statement's `items[].context_expr` is *not* a child of
/// `Stmt::Assign`, so legitimate `with open(...) as f:` forms never
/// trip the check — only bare assignments do.
fn check_resource_discipline(c: &mut Checker, body: &[Stmt]) {
    for stmt in body {
        check_resource_discipline_stmt(c, stmt);
    }
}

fn check_resource_discipline_stmt(c: &mut Checker, stmt: &Stmt) {
    match stmt {
        Stmt::Assign(a) => {
            if let Some(name) = dotted_callee_path(a.value.as_ref()) {
                if REQUIRE_WITH_CALLEES.iter().any(|p| *p == name) {
                    let span = (
                        a.value.range().start().to_usize(),
                        a.value.range().end().to_usize(),
                    );
                    c.diagnostics
                        .push_warning(TycError::resource_not_managed(
                            &name,
                            &c.path,
                            c.source,
                            span.0,
                            span.1.saturating_sub(span.0).max(1),
                        ));
                }
            }
        }
        Stmt::AnnAssign(a) => {
            if let Some(v) = a.value.as_ref() {
                if let Some(name) = dotted_callee_path(v.as_ref()) {
                    if REQUIRE_WITH_CALLEES.iter().any(|p| *p == name) {
                        let span = (
                            v.range().start().to_usize(),
                            v.range().end().to_usize(),
                        );
                        c.diagnostics
                            .push_warning(TycError::resource_not_managed(
                                &name,
                                &c.path,
                                c.source,
                                span.0,
                                span.1.saturating_sub(span.0).max(1),
                            ));
                    }
                }
            }
        }
        Stmt::FunctionDef(f) => check_resource_discipline(c, &f.body),
        Stmt::ClassDef(cd) => check_resource_discipline(c, &cd.body),
        Stmt::If(s) => {
            // `unsafe:` lowers to `if True:  # __typhon_unsafe__` at
            // preprocess time. Skip its body so deliberate
            // resource-leak escape hatches (`unsafe: let f =
            // open(...)`) don't trip the diagnostic.
            if c.is_unsafe_marker(s.range) {
                return;
            }
            check_resource_discipline(c, &s.body);
            for clause in &s.elif_else_clauses {
                check_resource_discipline(c, &clause.body);
            }
        }
        Stmt::For(s) => {
            check_resource_discipline(c, &s.body);
            check_resource_discipline(c, &s.orelse);
        }
        Stmt::While(s) => {
            check_resource_discipline(c, &s.body);
            check_resource_discipline(c, &s.orelse);
        }
        Stmt::With(s) => {
            // The items themselves are managed by definition.
            // Only walk the body.
            check_resource_discipline(c, &s.body);
        }
        Stmt::Try(s) => {
            check_resource_discipline(c, &s.body);
            for h in &s.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                check_resource_discipline(c, &h.body);
            }
            check_resource_discipline(c, &s.orelse);
            check_resource_discipline(c, &s.finalbody);
        }
        Stmt::Match(s) => {
            for case in &s.cases {
                check_resource_discipline(c, &case.body);
            }
        }
        _ => {}
    }
}

/// Walk the type-alias graph and emit `tyc::cyclic_type_alias` for any
/// alias whose chain returns to itself. FINDINGS #81. Operates on the
/// raw `Stmt::TypeAlias` declarations (not the resolved `type_aliases`
/// map) so the diagnostic span anchors at the original `type NAME = ...`
/// header.
fn detect_cyclic_type_aliases(c: &mut Checker, body: &[Stmt]) {
    use std::collections::HashMap;
    // Build `name -> set of alias names the RHS references`. We only
    // chase plain `Name` and `Generic` heads — nothing else can introduce
    // a self-referential alias cycle.
    fn collect_referenced_aliases(expr: &Expr, into: &mut std::collections::HashSet<String>) {
        match expr {
            Expr::Name(n) => {
                into.insert(n.id.as_str().to_owned());
            }
            Expr::Subscript(s) => {
                collect_referenced_aliases(&s.value, into);
                if let Expr::Tuple(t) = s.slice.as_ref() {
                    for elt in &t.elts {
                        collect_referenced_aliases(elt, into);
                    }
                } else {
                    collect_referenced_aliases(&s.slice, into);
                }
            }
            Expr::BinOp(b) => {
                collect_referenced_aliases(&b.left, into);
                collect_referenced_aliases(&b.right, into);
            }
            _ => {}
        }
    }
    let mut graph: HashMap<String, (std::collections::HashSet<String>, usize, usize)> =
        HashMap::new();
    for stmt in body {
        if let Stmt::TypeAlias(ta) = stmt {
            if let Expr::Name(n) = ta.name.as_ref() {
                let name = n.id.as_str().to_owned();
                let mut refs = std::collections::HashSet::new();
                collect_referenced_aliases(ta.value.as_ref(), &mut refs);
                let span_start = n.range.start().to_usize();
                let span_len = n.range.end().to_usize().saturating_sub(span_start).max(1);
                graph.insert(name, (refs, span_start, span_len));
            }
        }
    }
    // For each alias, DFS the graph looking for itself. A cycle exists iff
    // we can reach `start` from one of its referenced aliases.
    for (start, (_refs, span_start, span_len)) in &graph {
        let mut stack: Vec<&str> = vec![start.as_str()];
        let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut found_cycle = false;
        // Seed with the start's direct referents (don't insert `start` itself
        // into visited yet — we want to detect the case where the chain comes
        // back to it).
        let Some((direct_refs, _, _)) = graph.get(start) else {
            continue;
        };
        stack.clear();
        for r in direct_refs {
            stack.push(r.as_str());
        }
        while let Some(node) = stack.pop() {
            if node == start.as_str() {
                found_cycle = true;
                break;
            }
            if !visited.insert(node) {
                continue;
            }
            if let Some((next_refs, _, _)) = graph.get(node) {
                for r in next_refs {
                    stack.push(r.as_str());
                }
            }
        }
        if found_cycle {
            c.diagnostics.push_error(TycError::cyclic_type_alias(
                start.as_str(),
                &c.path,
                c.source,
                *span_start,
                *span_len,
            ));
            // The chain has no concrete type to resolve to, but the
            // alias is still referenced from value-position
            // annotations (`let x: JSON = ...`). Without this
            // override every such use cascades into a flood of
            // `tyc::type_mismatch` errors as the unifier compares
            // the recursive `Union[..., list[JSON], ...]` shape
            // against literal values it can never structurally
            // match. Replacing the alias body with `Any` keeps the
            // single, actionable `tyc::cyclic_type_alias` error and
            // silences the cascade so the rest of the file is
            // still checkable. FINDINGS O4.
            if let Some((params, _)) = c.type_aliases.get(start.as_str()).cloned() {
                c.type_aliases.insert(start.clone(), (params, Type::Any));
            }
        }
    }
}

fn collect_classes_and_functions(c: &mut Checker, body: &[Stmt]) {
    // First pass: collect every class and type-alias *name* into `c.classes`
    // so the subsequent shape and signature passes can resolve nominal
    // references like `field: OtherClass`. Doing the shape collection in
    // the same pass would see an empty class list and treat every nominal
    // type as `Unknown`.
    //
    // Track class-name first-sightings so a second declaration with the
    // same name fires `tyc::duplicate_class` (FINDINGS #77). Preprocessor-
    // synthesised pseudo-classes (`__typhon_impl_*`, `__TyphonLazy_*`) are
    // exempt: multiple `impl Foo:` blocks legitimately produce multiple
    // pseudo-classes, and the merge pass handles deduplication.
    let mut seen_class_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for stmt in body {
        match stmt {
            Stmt::ClassDef(cd) => {
                let name = cd.name.as_str().to_owned();
                if !name.starts_with("__typhon_impl_")
                    && !name.starts_with("__TyphonLazy_")
                    && !seen_class_names.insert(name.clone())
                {
                    let span_start = cd.name.range.start().to_usize();
                    let span_len = cd
                        .name
                        .range
                        .end()
                        .to_usize()
                        .saturating_sub(span_start)
                        .max(1);
                    c.diagnostics.push_error(TycError::duplicate_class(
                        name.as_str(),
                        &c.path,
                        c.source,
                        span_start,
                        span_len,
                    ));
                }
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
                        c.sealed_unions.insert(union_name.clone(), variants);
                    }
                    // Record the alias for transparent unwrap during
                    // assignability. We use a placeholder class list here —
                    // the second pass below rewrites the RHS once every
                    // class name is known.
                    let params = type_param_names_from(ta.type_params.as_deref());
                    c.type_aliases.insert(union_name, (params, Type::Unknown));
                }
            }
            // `newtype Name = Base` (preprocessed to `Name = NewType("Name",
            // Base)`) registers `Name` as a nominal type distinct from `Base`.
            // The classes list gets the name too so annotations referring to
            // it resolve to `Type::Class(name)`; the asymmetric base
            // relationship lives in `c.newtypes`.
            Stmt::Assign(a) if extract_newtype_decl(a).is_some() => {
                let (name, _base_expr) = extract_newtype_decl(a).expect("checked by guard");
                c.classes.push(name);
            }
            _ => {}
        }
    }
    let classes = c.classes.clone();
    // Resolve every newtype's base expression now that the class list is
    // populated. Done in this dedicated pass so a `newtype UserId = int`
    // that appears before its referenced class still resolves correctly.
    for stmt in body {
        if let Stmt::Assign(a) = stmt {
            if let Some((name, base_expr)) = extract_newtype_decl(a) {
                let base_ty = type_from_annotation_with_params(&base_expr, &classes, &[]);
                c.newtypes.insert(name, base_ty);
            }
        }
    }
    // Second pass: now that every class name is known, resolve each type
    // alias's RHS into a concrete `Type`. Doing this in pass 1 would
    // mis-translate forward references (e.g. `type Maybe[T] = Just[T] |
    // Nothing` declared before `class Just[T]:` / `class Nothing:`).
    // FINDINGS #57, #58, #70.
    for stmt in body {
        if let Stmt::TypeAlias(ta) = stmt {
            if let Expr::Name(n) = ta.name.as_ref() {
                let alias_name = n.id.as_str().to_owned();
                let params = type_param_names_from(ta.type_params.as_deref());
                let rhs = type_from_annotation_with_params(ta.value.as_ref(), &classes, &params);
                c.type_aliases.insert(alias_name, (params, rhs));
            }
        }
    }
    // FINDINGS #81: walk the alias graph and surface cycles. `unwrap_alias`
    // is bounded to 8 hops so a cycle silently produces an opaque type
    // instead of looping, which lets bad alias chains compile and emit
    // working Python that returns `Any` everywhere. A cycle is a
    // programming error: there is no concrete type the chain can resolve
    // to, so reject it at check time.
    detect_cyclic_type_aliases(c, body);
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
            c.class_shapes.insert(name.clone(), shape);
            // Record PEP 695 type-parameter names so call-site
            // inference can return `Type::Generic(name, [...])` for
            // generic classes (FINDINGS #46).
            let tps = type_param_names_from(cd.type_params.as_deref());
            if !tps.is_empty() {
                c.class_type_params.insert(name, tps);
            }
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
                    // Mirror `extract_module_shapes`: also merge
                    // `field_order` and `field_defaults` for newly
                    // contributed fields so an `impl X: y: int = 1`
                    // declaration is correctly recognised as
                    // defaulted at the constructor call site.
                    let impl_defaults = impl_shape.field_defaults;
                    for (f, ty) in impl_shape.fields {
                        let is_new = !target_shape.fields.contains_key(&f);
                        if is_new {
                            target_shape.field_order.push(f.clone());
                            if impl_defaults.contains(&f) {
                                target_shape.field_defaults.insert(f.clone());
                            }
                        }
                        target_shape.fields.entry(f).or_insert(ty);
                    }
                } else {
                    // FINDINGS #78: `impl UnknownClass:` silently produced
                    // dead code. Anchor the diagnostic on the class-name
                    // identifier (`__typhon_impl_X` byte range, with the
                    // synthetic prefix stripped) so the highlight covers
                    // the user-visible class name from `impl NAME:`.
                    let name_start = cd.name.range.start().to_usize();
                    let name_end = cd.name.range.end().to_usize();
                    let prefix_len = "__typhon_impl_".len();
                    let span_start = name_start.saturating_add(prefix_len);
                    let span_len = name_end.saturating_sub(span_start).max(1);
                    c.diagnostics.push_error(TycError::impl_unknown_class(
                        target, &c.path, c.source, span_start, span_len,
                    ));
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
                arity_info_from_parameters(f.parameters.as_ref(), &classes, &tps),
            );
            // Record `async def` names so the call-site arm can emit
            // `tyc::missing_await` for sync calls (FINDINGS #49). The
            // `async_without_await` warning is emitted from
            // `check_function` so the carve-outs (declaration-only
            // bodies, async generators, async-protocol dunders) live
            // alongside the body walk.
            if f.is_async {
                c.async_functions.insert(f.name.as_str().to_owned());
            }
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
                let is_static = f.decorator_list.iter().any(
                    |d| matches!(&d.expression, Expr::Name(n) if n.id.as_str() == "staticmethod"),
                );
                let is_classmethod = f.decorator_list.iter().any(
                    |d| matches!(&d.expression, Expr::Name(n) if n.id.as_str() == "classmethod"),
                );
                let arity = if is_static {
                    f.parameters.posonlyargs.len()
                        + f.parameters.args.len()
                        + f.parameters.kwonlyargs.len()
                } else {
                    method_arity_excluding_receiver(f.parameters.as_ref())
                };
                let return_type = match f.returns.as_deref() {
                    Some(r) => type_from_annotation(r, classes),
                    None => Type::Unknown,
                };
                let is_property = f.decorator_list.iter().any(|d| match &d.expression {
                    Expr::Name(n) => matches!(
                        n.id.as_str(),
                        "property" | "cached_property" | "_typhon_cached_property"
                    ),
                    Expr::Attribute(a) => matches!(a.attr.as_str(), "property" | "cached_property"),
                    _ => false,
                });
                let tps = type_param_names_from(f.type_params.as_deref());
                let full_arity = arity_info_from_parameters(f.parameters.as_ref(), classes, &tps);
                // Drop the implicit receiver (`self` / `cls`) from the
                // method's arity surface so call sites can match argument
                // counts directly. Static methods take no receiver.
                let arity_info = if is_static {
                    full_arity
                } else {
                    strip_receiver_from_arity(full_arity)
                };
                // Per-param types from annotations (used at call sites to
                // enforce real argument types — FINDINGS E2). Mirrors the
                // arity_info treatment: drop the leading `self` / `cls`
                // for instance / classmethods so positional indices line
                // up with `arity_info.param_names`.
                let mut param_types: Vec<Type> = f
                    .parameters
                    .posonlyargs
                    .iter()
                    .chain(f.parameters.args.iter())
                    .chain(f.parameters.kwonlyargs.iter())
                    .map(|p| match &p.parameter.annotation {
                        Some(ann) => type_from_annotation_with_params(ann, classes, &tps),
                        None => Type::Unknown,
                    })
                    .collect();
                if !is_static && !param_types.is_empty() {
                    param_types.remove(0);
                }
                shape.methods.insert(
                    f.name.as_str().to_owned(),
                    MethodSig {
                        arity,
                        return_type,
                        is_property,
                        is_static,
                        is_classmethod,
                        arity_info,
                        param_types,
                    },
                );
            }
            Stmt::AnnAssign(a) => {
                if let Expr::Name(n) = a.target.as_ref() {
                    let ty = type_from_annotation(&a.annotation, classes);
                    let name = n.id.as_str().to_owned();
                    if !shape.fields.contains_key(&name) {
                        shape.field_order.push(name.clone());
                    }
                    if a.value.is_some() {
                        shape.field_defaults.insert(name.clone());
                    }
                    shape.fields.insert(name, ty);
                }
            }
            _ => {}
        }
    }
    shape
}

/// Drop the first positional parameter (the implicit `self` / `cls`
/// receiver) from an [`ArityInfo`] computed over a raw method
/// signature. Used so a method like `def greet(self, prefix: str) ->
/// str` exposes a one-arg surface at the call site (`u.greet("hi")`)
/// instead of demanding two positional args.
fn strip_receiver_from_arity(mut info: ArityInfo) -> ArityInfo {
    if info.param_names.is_empty() {
        return info;
    }
    info.param_names.remove(0);
    if !info.required_positional.is_empty() {
        info.required_positional.remove(0);
    }
    info.min_positional = info.min_positional.saturating_sub(1);
    info.max_positional = info.max_positional.map(|n| n.saturating_sub(1));
    info
}

/// Build an [`ArityInfo`] modelling the auto-generated constructor of a
/// class declared with `class X:` (or `class X frozen:` / `model X:`).
/// Both `@dataclass` and Pydantic's `BaseModel` bind `__init__` arguments
/// to fields in declaration order, treating fields with an `= default`
/// as optional. Nullable fields (`T?`) without an explicit default are
/// NOT defaulted — Typhon does not auto-inject `= None`, matching the
/// emitted dataclass's runtime behaviour.
///
/// The returned `required_positional` records, per field, whether it is
/// required at construction — `true` for any field without an explicit
/// `= default`, regardless of declaration order. This is essential for
/// `model X:` (Pydantic) where a required field can legally follow a
/// defaulted one; the `@dataclass` form would already have failed at
/// class-definition time on the same shape, so the rule is conservative
/// in both directions.
///
/// An empty `param_names` (zero-field class) means the call site
/// short-circuits the arity check entirely.
/// Identify the constructor's required fields (no `= default`) that
/// weren't filled by the call site. Returns names in declaration
/// order — matches what the user sees scrolling the class definition.
///
/// A field is "filled" when it's either:
/// - Covered positionally (the call's `pos_args.len()`-th index in
///   `field_order` and below).
/// - Named in a kwarg.
///
/// Powers the `tyc::missing_argument` diagnostic. Returns an empty
/// vec — forcing the caller to fall back to `tyc::arg_count` — in
/// every case where the arity failure *isn't* reducible to "you
/// forgot field X":
///
/// - **Too many positionals** (`Point(1, 2, 3)` for a 2-field
///   class): the surplus arguments are the bug, not a missing
///   field.
/// - **Positional+kwarg double-binding** (`Point(1, x=2)`): the
///   conflict is the bug; saying "missing `y`" sends the user the
///   wrong fix.
/// - **`*iter` positional unpack** or **`**dict` kwarg unpack**:
///   the call's effective shape is unknowable statically, so any
///   missing-field guess could be wrong.
fn missing_required_fields(
    shape: &InterfaceShape,
    pos_args: &[Expr],
    kw_args: &[ruff_python_ast::Keyword],
) -> Vec<String> {
    let has_starred_pos = pos_args.iter().any(|e| matches!(e, Expr::Starred(_)));
    let has_double_star = kw_args.iter().any(|k| k.arg.is_none());
    if has_starred_pos || has_double_star {
        return Vec::new();
    }
    if pos_args.len() > shape.field_order.len() {
        // Too many positionals — surplus is the bug, not a missing
        // field. Fall back to the count-based diagnostic.
        return Vec::new();
    }
    let filled_positionally = pos_args.len().min(shape.field_order.len());
    let supplied_kwargs: std::collections::HashSet<&str> = kw_args
        .iter()
        .filter_map(|k| k.arg.as_ref().map(|i| i.as_str()))
        .collect();
    // Positional+kwarg double-binding: a kwarg names a field that
    // the positional args already filled. `check_arity_with_info`
    // flags this as `ArityCheck::Other`, but the *real* error is the
    // conflict — listing other unfilled fields as "missing" would
    // suggest the wrong fix.
    for (idx, name) in shape.field_order.iter().enumerate() {
        if idx < filled_positionally && supplied_kwargs.contains(name.as_str()) {
            return Vec::new();
        }
    }
    shape
        .field_order
        .iter()
        .enumerate()
        .filter(|(_, name)| !shape.field_defaults.contains(name.as_str()))
        .filter(|(idx, name)| {
            *idx >= filled_positionally && !supplied_kwargs.contains(name.as_str())
        })
        .map(|(_, name)| name.clone())
        .collect()
}

/// Free-function counterpart of [`missing_required_fields`]. Names
/// the required parameters (positional-without-default plus
/// kw-only-without-default) that weren't filled by the call.
///
/// Returns names in declaration order — positional params first,
/// then kw-only — so the diagnostic message reads in the same order
/// the user sees in the signature.
///
/// Returns an empty vec in the same set of "not really a
/// missing-field error" cases [`missing_required_fields`] handles
/// (too many positionals, positional+kwarg double-binding,
/// `*iter` / `**dict` unpacks) so the caller falls back to the
/// count-based `tyc::arg_count` diagnostic.
fn missing_required_params(
    info: &ArityInfo,
    pos_args: &[Expr],
    kw_args: &[ruff_python_ast::Keyword],
) -> Vec<String> {
    let has_starred_pos = pos_args.iter().any(|e| matches!(e, Expr::Starred(_)));
    let has_double_star = kw_args.iter().any(|k| k.arg.is_none());
    // A `*iter` positional or `**dict` keyword unpack expands to an
    // unknown number of arguments; we can't reason about specific
    // missing names so don't emit a misleading list. The caller falls
    // through to `wrong_args` in that case.
    if has_starred_pos || has_double_star {
        return Vec::new();
    }
    // Too many positionals (only meaningful when the function isn't
    // variadic; `*args` uncaps `max_positional` to `None`).
    if let Some(max_pos) = info.max_positional {
        if pos_args.len() > max_pos {
            return Vec::new();
        }
    }
    let supplied_kwargs: std::collections::HashSet<&str> = kw_args
        .iter()
        .filter_map(|k| k.arg.as_ref().map(|i| i.as_str()))
        .collect();
    // Positional+kwarg double-binding: a kwarg names a positional
    // parameter that the positional args already filled. The real
    // error is the conflict; suggesting another name as "missing"
    // would be misleading.
    let filled_positionally = pos_args.len().min(info.param_names.len());
    for name in info.param_names.iter().take(filled_positionally) {
        if supplied_kwargs.contains(name.as_str()) {
            return Vec::new();
        }
    }
    let use_per_param = info.required_positional.len() == info.param_names.len()
        && !info.required_positional.is_empty();
    let mut missing: Vec<String> = Vec::new();
    for (i, name) in info.param_names.iter().enumerate() {
        let required = if use_per_param {
            info.required_positional[i]
        } else {
            i < info.min_positional
        };
        if !required {
            continue;
        }
        if i < pos_args.len() {
            continue;
        }
        if supplied_kwargs.contains(name.as_str()) {
            continue;
        }
        missing.push(name.clone());
    }
    for name in &info.kwonly_required {
        if !supplied_kwargs.contains(name.as_str()) {
            missing.push(name.clone());
        }
    }
    missing
}

fn class_constructor_arity(shape: &InterfaceShape) -> ArityInfo {
    let max_positional = shape.field_order.len();
    let required_positional: Vec<bool> = shape
        .field_order
        .iter()
        .map(|name| !shape.field_defaults.contains(name))
        .collect();
    let min_positional = required_positional.iter().filter(|r| **r).count();
    ArityInfo {
        param_names: shape.field_order.clone(),
        min_positional,
        required_positional,
        max_positional: Some(max_positional),
        kwonly_names: Vec::new(),
        kwonly_required: Vec::new(),
        has_kwarg: false,
        vararg_type: None,
    }
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

/// Return `Some(name)` when `expr` is a bare container-type annotation
/// — `list`, `dict`, `tuple`, `set`, or `frozenset` written without a
/// subscript. These shapes carry an implicit `Any` element type and
/// violate Rule 1 / `[strictness] no-implicit-any = true` (FINDINGS
/// #72). Subscripted forms (`list[int]`, `dict[str, int]`) and bare
/// names that aren't container types return `None`.
fn bare_collection_name(expr: &Expr) -> Option<&'static str> {
    match expr {
        Expr::Name(n) => match n.id.as_str() {
            "list" => Some("list"),
            "dict" => Some("dict"),
            "tuple" => Some("tuple"),
            "set" => Some("set"),
            "frozenset" => Some("frozenset"),
            _ => None,
        },
        _ => None,
    }
}

/// Build the `help:` string for a `tyc::unknown_kwarg` diagnostic
/// (FINDINGS #80). If any candidate is "close enough" by character-level
/// edit distance, suggest it; otherwise list every accepted parameter
/// name so users see what's available.
fn suggest_candidate(typo: &str, candidates: &[String]) -> String {
    let best = candidates
        .iter()
        .filter_map(|c| {
            let d = levenshtein(typo, c);
            // Threshold: at most one third of the longer name, capped at 3.
            let max_d = (typo.len().max(c.len()) / 3).clamp(1, 3);
            if d <= max_d {
                Some((d, c.as_str()))
            } else {
                None
            }
        })
        .min_by_key(|(d, _)| *d)
        .map(|(_, name)| name);
    match best {
        Some(c) => format!("did you mean `{c}`?"),
        None if candidates.is_empty() => "this function takes no keyword arguments".to_string(),
        None => {
            let names: Vec<&str> = candidates.iter().map(|s| s.as_str()).collect();
            format!("accepted parameters: {}", names.join(", "))
        }
    }
}

/// Plain Levenshtein edit distance over byte-level characters. Adequate
/// for the short identifier strings used by `suggest_candidate`.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr: Vec<usize> = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        curr[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Resolve the `ArityInfo` for a method-call target of the form
/// `<receiver>.<method>(...)`. Walks the receiver's static type to find
/// the matching [`MethodSig`] and returns its stored `arity_info`,
/// which already excludes the implicit `self` / `cls` receiver. Returns
/// `None` when:
///
/// - the receiver isn't a known class instance (e.g. `Any`, a foreign
///   type from `unsafe:`, or an unresolved name),
/// - the attribute doesn't name a method on that class, or
/// - the receiver IS the class name (`User.greet`, an unbound-method
///   access) — those call sites would need to fill `self` themselves and
///   are still handled by the legacy permissive shape until we model
///   them properly.
///
/// Class-qualified calls into `@classmethod` and `@staticmethod`
/// targets are accepted because the arity info already accounts for
/// the absent (or auto-bound) receiver.
fn method_arity_info_for_attribute(
    c: &Checker,
    attr: &ruff_python_ast::ExprAttribute,
) -> Option<ArityInfo> {
    // The same Expr::Attribute resolution path the call site uses to
    // pick a method's return type, mirrored here so we can grab the
    // richer arity surface that `Type::Function` discards.
    let recv = infer_expr_readonly(c, &attr.value);
    let class_name = match &recv {
        Type::Class(n) => n.clone(),
        Type::Generic(n, _) => n.clone(),
        _ => return None,
    };
    let receiver_is_class_name = matches!(
        attr.value.as_ref(),
        Expr::Name(n) if c.classes.iter().any(|cn| cn == n.id.as_str())
    );
    let sig = c.find_method(&class_name, attr.attr.as_str())?;
    if sig.is_property {
        return None;
    }
    // For `ClassName.method(instance, ...)` (unbound-method form),
    // an extra positional is required for the receiver. Modelling
    // that re-adds the `self` slot to `param_names`; without it the
    // call would falsely arity-fail. Keep the existing permissive
    // shape there for now to avoid regressing class-qualified calls.
    if receiver_is_class_name && !sig.is_static && !sig.is_classmethod {
        return None;
    }
    Some(sig.arity_info.clone())
}

/// A side-effect-free variant of [`infer_expr`] that walks the same
/// match arms but avoids emitting diagnostics. Used by
/// [`method_arity_info_for_attribute`] to peek at the receiver's static
/// type before the surrounding call-site machinery walks it for real.
/// Covers `Name`, `Attribute`, and `Call` so chained shapes like
/// `get_client().method(...)` correctly resolve the method's arity;
/// without the `Expr::Call` arm, the chained-call receiver would fall
/// back to `Type::Unknown` and the method arity check would silently
/// skip. FINDINGS — gemini review of v0.2.0.
fn infer_expr_readonly(c: &Checker, e: &Expr) -> Type {
    match e {
        Expr::Name(n) => c
            .env
            .lookup(n.id.as_str())
            .map(|b| b.narrowed.clone())
            .unwrap_or(Type::Unknown),
        Expr::Attribute(a) => {
            let recv = infer_expr_readonly(c, &a.value);
            match &recv {
                Type::Class(class_name) | Type::Generic(class_name, _) => {
                    if let Some(field_ty) = c.find_field(class_name, a.attr.as_str()) {
                        field_ty.clone()
                    } else if let Some(sig) = c.find_method(class_name, a.attr.as_str()) {
                        if sig.is_property {
                            sig.return_type.clone()
                        } else {
                            Type::Unknown
                        }
                    } else {
                        Type::Unknown
                    }
                }
                Type::Module(mod_name) => {
                    // Foreign module attribute access — consult the
                    // module registry so a chained
                    // `clients.ApiClient(...).url(...)` can see the
                    // return type of the constructor (the class) and
                    // then resolve `url` on it.
                    if let Some(shapes) = c.module_registry.get(mod_name) {
                        if shapes.class_shapes.contains_key(a.attr.as_str()) {
                            Type::Class(format!("{}.{}", mod_name, a.attr.as_str()))
                        } else {
                            Type::Unknown
                        }
                    } else {
                        Type::Unknown
                    }
                }
                _ => Type::Unknown,
            }
        }
        Expr::Call(call) => {
            // Resolve the callee's return type so chained call
            // expressions (`get_client().method(...)`) can walk
            // through. A constructor call returning `Type::Class(_)`
            // is what we usually care about here; free-function
            // calls returning `Type::Unknown` simply fall through
            // and the outer caller uses the permissive shape.
            let func_ty = infer_expr_readonly(c, &call.func);
            match func_ty {
                Type::Class(name) => Type::Class(name),
                Type::Function { ret, .. } => *ret,
                _ => Type::Unknown,
            }
        }
        _ => Type::Unknown,
    }
}

/// Outcome of an arity check against a function's [`ArityInfo`]. The
/// specific failure mode is preserved so the caller can pick the right
/// diagnostic — e.g. `UnknownKwarg` produces a friendlier
/// `tyc::unknown_kwarg` instead of an arg-count miscount (FINDINGS #80).
#[derive(Debug)]
enum ArityCheck {
    Ok,
    /// A keyword argument was passed whose name doesn't match any
    /// parameter (and the function doesn't have `**kwargs`).
    UnknownKwarg {
        /// The offending kwarg name (e.g. `"greetinx"`).
        name: String,
        /// All parameter names accepted by the function. Used to suggest
        /// the closest match in the diagnostic.
        candidates: Vec<String>,
        /// The kw arg's source position.
        span: (usize, usize),
    },
    /// Any other arity failure (count mismatch, double-bound name,
    /// missing required, etc.). Falls back to `tyc::arg_count`.
    Other,
}

/// Decide whether a call site's positional + keyword arguments are
/// compatible with the named function's [`ArityInfo`]. Returns
/// [`ArityCheck::Ok`] for a sound call, or the specific failure mode
/// otherwise — the caller picks the right diagnostic.
///
/// Rules:
/// 1. Every kw argument must either match a parameter name (positional
///    or kw-only), or be absorbed by `**kwargs`. Mismatch →
///    [`ArityCheck::UnknownKwarg`].
/// 2. A parameter can be filled by either a positional or a kw arg —
///    not both. Conflicts → [`ArityCheck::Other`].
/// 3. Every required positional / kw-only param must be filled.
/// 4. The total positional count must not exceed `max_positional`
///    (unless the function has `*args`, in which case `max_positional`
///    is `None`).
fn check_arity_with_info(
    info: &ArityInfo,
    pos_args: &[Expr],
    kw_args: &[ruff_python_ast::Keyword],
) -> ArityCheck {
    // Filter out the `**double-star` unpacking keywords (`kw.arg == None`) — we
    // can't statically know how many keys they contain, so we treat them as
    // matching anything (kwarg sentinel).
    let named_kwargs: Vec<&str> = kw_args
        .iter()
        .filter_map(|k| k.arg.as_ref().map(|i| i.as_str()))
        .collect();
    let has_double_star = kw_args.iter().any(|k| k.arg.is_none());
    // A `*iter` positional unpack expands to an unknown number of
    // positionals; we can't reason about specific positional slots so
    // the arity check degrades to "trust the user". FINDINGS #109.
    let has_starred_positional = pos_args.iter().any(|e| matches!(e, Expr::Starred(_)));

    // Rule 4: positional count must fit max_positional (None → unbounded).
    if let Some(max) = info.max_positional {
        if !has_starred_positional && pos_args.len() > max {
            return ArityCheck::Other;
        }
    }

    // Rule 1: every named kw must hit a parameter (or `**kwargs`).
    // Surface the first offender with a dedicated variant so the
    // caller emits `tyc::unknown_kwarg` instead of a confusing
    // `tyc::arg_count` (FINDINGS #80).
    if !info.has_kwarg {
        for kw in kw_args {
            let Some(ident) = &kw.arg else { continue };
            let name = ident.as_str();
            let hits_pos = info.param_names.iter().any(|p| p == name);
            let hits_kwonly = info.kwonly_names.iter().any(|p| p == name);
            if !hits_pos && !hits_kwonly {
                let mut candidates = info.param_names.clone();
                candidates.extend(info.kwonly_names.iter().cloned());
                let span = (
                    ident.range.start().to_usize(),
                    ident.range.start().to_usize() + name.len(),
                );
                return ArityCheck::UnknownKwarg {
                    name: name.to_owned(),
                    candidates,
                    span,
                };
            }
        }
    }

    // Rule 2: a positional-bound name can't also appear as a kw.
    let filled_positionally = pos_args.len().min(info.param_names.len());
    for name in &named_kwargs {
        if info.param_names[..filled_positionally]
            .iter()
            .any(|p| p == name)
        {
            return ArityCheck::Other;
        }
    }

    // Rule 3a: every required positional must be filled by a pos arg or
    // matching kw arg. Stops being checkable when `**kwargs` unpacking is
    // present — in that case we trust the user. A `*positional` unpack
    // also expands to an unknown number of args, so skip the count
    // check there too (FINDINGS #109).
    //
    // When the caller populated `required_positional` (per-param
    // required flags, parallel to `param_names`) we honour it directly
    // so a `model X: id: int = 1; name: str` constructor correctly
    // requires `name` even though it follows a defaulted field
    // (FINDINGS — codex review of v0.2.0). Free-function arity
    // construction populates `required_positional` such that the
    // result is identical to "first `min_positional` are required",
    // so the new path subsumes the old one. The
    // `required_positional.is_empty()` fallback preserves behaviour
    // when an external caller built an `ArityInfo` without the new
    // field — only the per-class synthesis and `arity_info_from_parameters`
    // ever populate it in-tree.
    if !has_double_star && !has_starred_positional {
        let use_per_param = info.required_positional.len() == info.param_names.len()
            && !info.required_positional.is_empty();
        if use_per_param {
            for (i, p) in info.param_names.iter().enumerate() {
                if !info.required_positional[i] {
                    continue;
                }
                if i < pos_args.len() {
                    continue;
                }
                if named_kwargs.iter().any(|kw| kw == p) {
                    continue;
                }
                return ArityCheck::Other;
            }
        } else {
            for (i, p) in info
                .param_names
                .iter()
                .enumerate()
                .take(info.min_positional)
            {
                if i < pos_args.len() {
                    continue;
                }
                if named_kwargs.iter().any(|kw| kw == p) {
                    continue;
                }
                return ArityCheck::Other;
            }
        }
        // Rule 3b: every required kw-only must be filled.
        for required in &info.kwonly_required {
            if !named_kwargs.iter().any(|kw| kw == required) {
                return ArityCheck::Other;
            }
        }
    }
    ArityCheck::Ok
}

/// Compute the [`ArityInfo`] sidecar for a `def`'s parameter list.
///
/// Walks the same positional / keyword / vararg / kwarg shape as
/// `function_signature` but extracts the metadata that doesn't fit on
/// `Type::Function` (param names for keyword-arg matching, count of
/// defaulted params for the min-arity bound, kw-only requireds, and
/// the `**kwargs` flag).
fn arity_info_from_parameters(
    parameters: &ruff_python_ast::Parameters,
    classes: &[String],
    type_params: &[String],
) -> ArityInfo {
    let mut param_names: Vec<String> = Vec::new();
    let mut required_positional: Vec<bool> = Vec::new();
    let mut min_positional: usize = 0;
    // Walk positional-only + positional-or-keyword. A defaulted positional
    // doesn't count toward `min_positional`; once we see the first
    // defaulted param all subsequent positionals must also be defaulted
    // (Python grammar enforces this), so we can stop incrementing once
    // we encounter a default. `required_positional` records the same
    // information per-param so the call-site arity check can honour
    // shapes whose defaults aren't all trailing (e.g. synthesised
    // Pydantic-model constructors).
    let positional_chain = parameters.posonlyargs.iter().chain(parameters.args.iter());
    let mut hit_default = false;
    let mut max_positional_count: usize = 0;
    for pwd in positional_chain {
        param_names.push(pwd.parameter.name.as_str().to_owned());
        max_positional_count += 1;
        let has_default = pwd.default.is_some();
        if !has_default && !hit_default {
            min_positional += 1;
            required_positional.push(true);
        } else {
            hit_default = true;
            required_positional.push(false);
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
    // FINDINGS #86: record the *args element type so the call-site
    // type-check can stop misapplying the next positional-or-kw
    // parameter's annotation to absorbed extra positional args.
    let vararg_type = parameters.vararg.as_ref().map(|va| {
        va.annotation
            .as_ref()
            .map(|ann| type_from_annotation_with_params(ann, classes, type_params))
            .unwrap_or(Type::Unknown)
    });
    ArityInfo {
        param_names,
        min_positional,
        required_positional,
        max_positional,
        kwonly_names,
        kwonly_required,
        has_kwarg: parameters.kwarg.is_some(),
        vararg_type,
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
            // Cross-module class imports: when the caller seeded a
            // class shape for this local name via
            // `check_module_with_imports`, treat the binding as a
            // `Type::Class` so call sites flow through the constructor
            // arity check. Function imports get the same treatment via
            // `function_arity_info` further down. Without this, a
            // `from foo import ApiClient` would land as
            // `Type::Unknown` and `ApiClient(...)` calls would
            // bypass the new arity check.
            //
            // For bare `import M as N` (no `member`), the binding is
            // given `Type::Module(dotted)` so attribute access
            // (`N.SomeClass(...)`) can consult `module_registry` for
            // the foreign class shape. The dotted module name comes
            // straight from the resolver's `ImportInfo`.
            BindingKind::Import => {
                if c.class_shapes.contains_key(&b.name) {
                    Type::Class(b.name.clone())
                } else if let Some(info) = c.function_arity_info.get(&b.name) {
                    // Build a `Type::Function` from the cross-module
                    // arity so the call-site machinery can pick it
                    // up the same way it picks up local `def`s.
                    let params = vec![Type::Unknown; info.param_names.len()];
                    Type::Function {
                        params,
                        ret: Box::new(Type::Unknown),
                        variadic: info.max_positional.is_none(),
                    }
                } else if let Some(info) = &b.import_info {
                    if info.member.is_none() && c.module_registry.contains_key(&info.module) {
                        Type::Module(info.module.clone())
                    } else {
                        Type::Unknown
                    }
                } else {
                    Type::Unknown
                }
            }
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
            // FINDINGS #72: a bare `list` / `dict` / `tuple` / `set` /
            // `frozenset` annotation has an implicit `Any` element type
            // and violates Rule 1. Class-body field declarations also
            // route through `Stmt::AnnAssign` but their annotations
            // should be checked too — `name: list` is just as opaque
            // inside a class as outside one.
            if let Some(bare) = bare_collection_name(&a.annotation) {
                let span = (
                    a.annotation.range().start().to_usize(),
                    a.annotation.range().end().to_usize(),
                );
                let length = span.1.saturating_sub(span.0).max(1);
                c.diagnostics.push_error(TycError::implicit_any(
                    bare,
                    c.path.clone(),
                    c.source,
                    span.0,
                    length,
                ));
            }
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
                // Audit hook: `c.field = ...` writes mark the field
                // assigned on a tracked bypass-constructed binding.
                audit_record_field_set(c, a.target.as_ref());
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
                // Audit hook: register or refresh the binding's
                // bypass-construction tracking based on the RHS.
                if let Some(value) = &a.value {
                    if let Some(class) = detect_new_bypass(value) {
                        audit_register_bypass(c, n.id.as_str(), &class);
                    } else {
                        audit_clear_binding(c, n.id.as_str());
                    }
                }
            }
        }
        Stmt::Assign(a) => {
            let value_type = infer_expr(c, &a.value);
            for target in &a.targets {
                check_attr_assign_not_frozen(c, target);
                audit_record_field_set(c, target);
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
                    // Audit: detect `c = X.__new__(X)` shape and
                    // register / clear tracking accordingly.
                    if let Some(class) = detect_new_bypass(&a.value) {
                        audit_register_bypass(c, n.id.as_str(), &class);
                    } else {
                        audit_clear_binding(c, n.id.as_str());
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
                (
                    f.name.as_str(),
                    f.name.range.start().to_usize(),
                    f.name.as_str().len().max(1),
                ),
                f.parameters.as_ref(),
                &f.body,
                f.returns.as_deref(),
                &tps,
                f.is_async,
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
                let merged_methods_empty = c
                    .class_shapes
                    .get(class_name)
                    .map(|s| s.methods.is_empty())
                    .unwrap_or(true);
                let body_has_function = cd.body.iter().any(|s| matches!(s, Stmt::FunctionDef(_)));
                let mut has_ann_assign = false;
                let mut all_ann_assigns_defaulted = true;
                let mut only_ann_assigns = true;
                let mut first_defaulted: Option<(&ruff_python_ast::StmtAnnAssign, String)> = None;
                for s in &cd.body {
                    match s {
                        Stmt::AnnAssign(a) => {
                            has_ann_assign = true;
                            if a.value.is_none() {
                                all_ann_assigns_defaulted = false;
                            } else if first_defaulted.is_none() {
                                if let Expr::Name(n) = a.target.as_ref() {
                                    first_defaulted = Some((a, n.id.as_str().to_owned()));
                                }
                            }
                        }
                        Stmt::Pass(_) => {}
                        Stmt::Expr(e)
                            if matches!(
                                e.value.as_ref(),
                                Expr::StringLiteral(_) | Expr::EllipsisLiteral(_)
                            ) => {}
                        _ => only_ann_assigns = false,
                    }
                }
                if has_ann_assign
                    && all_ann_assigns_defaulted
                    && only_ann_assigns
                    && !body_has_function
                    && merged_methods_empty
                {
                    if let Some((ann, field_name)) = first_defaulted {
                        let value_hint = ann
                            .value
                            .as_deref()
                            .map(|v| match v {
                                Expr::StringLiteral(s) => {
                                    format!("\"{}\"", s.value.to_str())
                                }
                                Expr::NumberLiteral(n) => match &n.value {
                                    ruff_python_ast::Number::Int(i) => i.to_string(),
                                    ruff_python_ast::Number::Float(f) => f.to_string(),
                                    ruff_python_ast::Number::Complex { real, imag } => {
                                        format!("{}+{}j", real, imag)
                                    }
                                },
                                Expr::BooleanLiteral(b) => {
                                    if b.value {
                                        "True".to_owned()
                                    } else {
                                        "False".to_owned()
                                    }
                                }
                                _ => "its literal value".to_owned(),
                            })
                            .unwrap_or_else(|| "its literal value".to_owned());
                        let class_range = cd.name.range;
                        c.diagnostics
                            .push_warning(TycError::class_attr_shadows_slot(
                                class_name.to_owned(),
                                field_name,
                                value_hint,
                                c.path.clone(),
                                c.source,
                                class_range.start().to_usize(),
                                class_name.len().max(1),
                            ));
                    }
                }
                for s in &cd.body {
                    if let Stmt::FunctionDef(f) = s {
                        let method = f.name.as_str();
                        let span_start = f.name.range.start().to_usize();
                        // `__init__` is generated from the field
                        // annotations (`@dataclass(slots=True)` or
                        // `BaseModel`). Writing one manually conflicts
                        // with the emitted constructor and is rejected
                        // by the docs (FINDINGS #50). Emit a dedicated
                        // `tyc::manual_init` error here rather than
                        // falling through to the softer
                        // `method_in_class_body` warning.
                        if method == "__init__" {
                            c.diagnostics.push_error(TycError::manual_init(
                                class_name.to_owned(),
                                c.path.clone(),
                                c.source,
                                span_start,
                                method.len(),
                            ));
                            continue;
                        }
                        // Don't warn on dunders the user is *expected* to
                        // override (e.g. `__add__`, `__lt__`); those are
                        // legitimate uses of class-body methods too. The
                        // canonical bad case is user-named methods like
                        // `draw`, `display`, `is_admin`.
                        if method.starts_with("__") && method.ends_with("__") {
                            continue;
                        }
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
            // Audit: a `return c` where `c` was bypass-constructed
            // and has unassigned required fields is the canonical
            // escape we want to catch.
            if let Some(ret_expr) = &ret.value {
                audit_check_escape(c, ret_expr);
            }
            // Inside a generator function body, `return value` is
            // shorthand for `raise StopIteration(value)` — the value
            // becomes the *generator's* return payload, not an
            // `Iterator[T]`. The check has three shapes (FINDINGS O6
            // + Codex review feedback on PR #94):
            //
            //   * `-> Iterator[T]` / `-> Iterable[T]` / async variants
            //     don't expose a return-type parameter at all, so we
            //     accept any `return value` — the payload is
            //     effectively `None` from the user's perspective.
            //   * `-> Generator[Y, S, R]` carries the return payload's
            //     declared type as the third parameter; check the
            //     return value against `R` instead of skipping. This
            //     restores the type-safety the early-return shortcut
            //     would otherwise lose.
            //   * Bare `return` (no value) is always fine — it
            //     produces `StopIteration()` with no payload, which
            //     is the standard `break-out-of-generator` shape.
            if c.in_generator {
                let generator_return_type = c
                    .current_return
                    .as_ref()
                    .and_then(extract_generator_return_type);
                if let (Some(ret_expr), Some(expected_r)) =
                    (&ret.value, generator_return_type.clone())
                {
                    let value_type = infer_expr_ctx(c, ret_expr, Some(&expected_r));
                    if !matches!(expected_r, Type::Unknown)
                        && !c.is_assignable(&expected_r, &value_type)
                    {
                        let span = (
                            ret_expr.range().start().to_usize(),
                            ret_expr.range().end().to_usize(),
                        );
                        c.mismatch(&expected_r, &value_type, span);
                    }
                } else if let Some(ret_expr) = &ret.value {
                    // `Iterator[T]` / `Iterable[T]` / async variants —
                    // no R parameter to check against. Still walk the
                    // expression so name uses are validated.
                    let _ = infer_expr(c, ret_expr);
                }
                return;
            }
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
            // Flow narrowing inside the loop body: the test is known to
            // hold on every iteration that *enters* the body, so the
            // narrowing it implies is sound at the top of each pass
            // through the body (FINDINGS O2). The pattern that matters
            // is the linked-list iterator
            //   while cur is not None:
            //       total += cur.value
            //       cur = cur.next
            // — without this, `cur.value` would trip `tyc::nullable_use`
            // even though the loop test guarantees the value is non-null
            // at the read site. A subsequent `cur = cur.next` reassignment
            // resets narrowing at the assignment site (see the bareword
            // assignment arm above), so a later iteration that reads the
            // *new* value with the *old* narrowing is impossible — by
            // the time control flows back to the head of the loop the
            // narrowing snapshot has been restored.
            let narrowings = collect_narrowings(c, &w.test, /*negate=*/ false);
            let snap_pre = c.env.snapshot();
            apply_narrowings(c, &narrowings);
            for s in &w.body {
                check_stmt(c, s);
            }
            c.env.restore(snap_pre);
            // `while ... else:` runs exactly when the loop test became
            // false without a `break`, so the negated narrowing holds
            // at the top of the orelse block — the dual of the
            // positive narrowing applied to the body. Mirrors the
            // `if` checker's else-branch handling.
            let neg = collect_narrowings(c, &w.test, /*negate=*/ true);
            let snap_pre = c.env.snapshot();
            apply_narrowings(c, &neg);
            for s in &w.orelse {
                check_stmt(c, s);
            }
            c.env.restore(snap_pre);
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
                check_pattern_class_fields(c, &case.pattern);
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
    name_info: (&str, usize, usize),
    parameters: &ruff_python_ast::Parameters,
    body: &[Stmt],
    returns: Option<&Expr>,
    type_params: &[String],
    is_async: bool,
) {
    let (name, name_span_offset, name_span_len) = name_info;
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

    // `yield` inside a non-iterator-typed function (FINDINGS #51): a
    // body containing `yield` / `yield from` produces a generator at
    // runtime, so the declared return type must match an iterator
    // shape. Auto-synthesised helpers are exempt for the same reason
    // as Rule 1.
    if !name.starts_with("__typhon_") {
        if let Some(returns_expr) = returns {
            if body_has_yield(body) && !is_iterator_return_type(returns_expr, is_async) {
                let span = (
                    returns_expr.range().start().to_usize(),
                    returns_expr.range().end().to_usize(),
                );
                let length = span.1.saturating_sub(span.0).max(1);
                let returned = match returns_expr {
                    Expr::Name(n) => n.id.as_str().to_owned(),
                    _ => "the declared type".to_owned(),
                };
                c.diagnostics.push_error(TycError::generator_return_type(
                    name,
                    returned,
                    c.path.clone(),
                    c.source,
                    span.0,
                    length,
                ));
            }
        }
    }

    // `async def` with no `await` — warn per tyc::async_without_await (FINDINGS #83).
    // Only fires for user-authored async functions, not compiler-synthesised helpers
    // (prefixed with `__typhon_`). Carve-outs: async-protocol dunders
    // (`__aenter__`/`__aexit__`/`__aiter__`/`__anext__`), declaration-only
    // bodies (`...` / `pass` / docstring) that look like Protocol/interface
    // signatures, and async generators (bodies that `yield`).
    if is_async
        && !name.starts_with("__typhon_")
        && !is_async_protocol_dunder(name)
        && !body_has_await(body)
        && !body_is_declaration_only(body)
        && !body_has_yield(body)
    {
        c.diagnostics.push_warning(TycError::async_without_await(
            name,
            c.path.clone(),
            c.source,
            name_span_offset,
            name_span_len,
        ));
    }

    let saved_return = c.current_return.replace(ret_type.clone());
    // Track sync-vs-async for the body's call-site check
    // (`tyc::missing_await` — FINDINGS #49). Only sync function bodies
    // trip the diagnostic; async bodies use `await` naturally and
    // module-level scope keeps the `asyncio.run(coro())` entry-point
    // pattern free of false positives.
    let saved_in_sync = c.in_sync_function;
    c.in_sync_function = !is_async;
    let saved_in_async = c.in_async_function;
    c.in_async_function = is_async;
    // Track whether the current function body is a generator so the
    // return-statement validator can skip the usual assignability
    // check (FINDINGS O6): inside a generator, `return` raises
    // `StopIteration`, not a value against the declared `Iterator[T]`.
    let saved_in_generator = c.in_generator;
    c.in_generator = body_has_yield(body);
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
                            // `impl X:` / `extend X:` desugar to a
                            // `__typhon_impl_X` pseudo-class that the
                            // merge pass later folds into the real `X`.
                            // The type-checker walks the methods inside
                            // the pseudo-class first, so `self` would
                            // pick up the pseudo-class as its receiver
                            // type and `return self` against a declared
                            // `-> X` would fail with
                            // `expected X, found __typhon_impl_X`
                            // (FINDINGS E3). Strip the prefix so the
                            // receiver carries the user-facing class
                            // name from the start.
                            Some(cls) => {
                                let real =
                                    cls.strip_prefix("__typhon_impl_").unwrap_or(cls).to_owned();
                                Type::Class(real)
                            }
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

    // Missing-return analysis (FINDINGS #82): when the declared return
    // type cannot accommodate `None` and the body is not a generator
    // (yield → iterator), every path through the function must end in
    // `return` / `raise`. Auto-synthesised compiler helpers are exempt
    // for the same reason as Rule 1 / Rule 4. Stub bodies (`...` /
    // `pass` / docstring-only) are also exempt because that's the
    // canonical shape for `interface` (Protocol) methods and abstract
    // base-class declarations — both legitimately "don't return".
    if !name.starts_with("__typhon_")
        && returns.is_some()
        && return_type_requires_value(&ret_type)
        && !body_has_yield(body)
        && !body_is_stub(body)
        && !body_always_exits_aware(c, body)
    {
        c.diagnostics.push_error(TycError::missing_return(
            name,
            ret_type.display(),
            c.path.clone(),
            c.source,
            name_span_offset,
            name_span_len,
        ));
    }

    c.env.leave();
    c.current_return = saved_return;
    c.active_typevar_bounds = saved_bounds;
    c.in_sync_function = saved_in_sync;
    c.in_async_function = saved_in_async;
    c.in_generator = saved_in_generator;
}

/// True when `body` is a stub — `...`, `pass`, or a single docstring
/// followed by one of those. Used to exempt `interface` (Protocol)
/// method declarations and abstract base-class methods from the
/// missing-return check (FINDINGS #82).
fn body_is_stub(body: &[Stmt]) -> bool {
    let stmts: Vec<&Stmt> = body.iter().filter(|s| !is_docstring_stmt(s)).collect();
    match stmts.as_slice() {
        [Stmt::Pass(_)] => true,
        [Stmt::Expr(e)] => matches!(e.value.as_ref(), Expr::EllipsisLiteral(_)),
        [] => true, // body was nothing but docstrings
        _ => false,
    }
}

/// True when this statement is a bare string-literal expression — the
/// canonical "docstring" shape that lives at the top of a function /
/// class body.
fn is_docstring_stmt(stmt: &Stmt) -> bool {
    matches!(
        stmt,
        Stmt::Expr(e) if matches!(e.value.as_ref(), Expr::StringLiteral(_))
    )
}

/// True when the declared return type can NOT silently accept a missing
/// `return` (i.e. the implicit `return None` Python uses to fall off the
/// end of a function). Anything that includes `None` in its surface
/// (literal `None`, `T?` / `T | None`, the bare `Unknown` for unannotated
/// returns, or `Type::Any`) is exempt — those are the cases where falling
/// off the end is a legal value.
fn return_type_requires_value(t: &Type) -> bool {
    match t {
        Type::Unknown | Type::Any | Type::None => false,
        Type::Union(variants) => !variants.iter().any(|v| matches!(v, Type::None)),
        _ => true,
    }
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
        // `if True:` (which is what `unsafe:` lowers to) collapses to
        // "body always runs", so it exits iff the body exits.
        Stmt::If(s) => {
            if is_constant_true(&s.test) {
                return body_always_exits(&s.body);
            }
            body_always_exits(&s.body) && elif_else_chain_always_exits(&s.elif_else_clauses)
        }
        // A `with` / `async with` block exits the enclosing function only
        // when every terminal in its body is *non-suppressible* —
        // `return` / `break` / `continue`. Bare `raise` is suppressible
        // by the context manager's `__exit__` (e.g.
        // `contextlib.suppress(Exception)`), so a `with: raise` body
        // is not a definite function exit even though the statement
        // itself "exits" the with-block.
        Stmt::With(w) => body_exits_non_suppressible(&w.body),
        // A `match` statement where every arm body exits, including a
        // wildcard / capturing fallback that handles unmatched values,
        // is itself an exit. Exhaustive sealed-union matches already
        // have a `case _:` arm (the exhaustiveness pass would have
        // rejected non-exhaustive ones), so this matches the user's
        // intuition that a covered match doesn't fall through.
        Stmt::Match(m) => match_arms_always_exit(&m.cases),
        // A `try/except` always exits when:
        //   (a) the `finally` body is non-empty and always exits (finally runs
        //       on every path), OR
        //   (b) the `try` body always exits AND every exception handler always
        //       exits AND the `else` clause (if present) always exits.
        // Case (b) is the common `try: return Ok(x) except E: return Err(e)`
        // pattern that must not be flagged as a missing-return false positive.
        Stmt::Try(t) => {
            let finally_exits = !t.finalbody.is_empty() && body_always_exits(&t.finalbody);
            let try_and_handlers_exit = body_always_exits(&t.body)
                && t.handlers.iter().all(|h| {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    body_always_exits(&h.body)
                })
                && (t.orelse.is_empty() || body_always_exits(&t.orelse));
            finally_exits || try_and_handlers_exit
        }
        _ => false,
    }
}

/// Structural-only check: do every arm's body end in an unconditional
/// exit, and is there at least one arm? Used by the narrowing pass in
/// `check_if` (where over-approximating "exit" is harmless — it just
/// loses narrowing precision, never produces wrong types). For
/// missing-return analysis, use [`match_arms_always_exit_aware`]
/// instead — that variant requires either a catch-all arm or proven
/// sealed-union exhaustiveness, since a non-exhaustive open match can
/// legitimately fall through (Copilot review on PR #68, file
/// tyc-types/src/lib.rs L3558).
fn match_arms_always_exit(cases: &[ruff_python_ast::MatchCase]) -> bool {
    !cases.is_empty() && cases.iter().all(|c| body_always_exits(&c.body))
}

/// Checker-aware variant of [`match_arms_always_exit`]: returns true only
/// when the match definitively covers every reachable value. Used by the
/// missing-return analysis, where falsely claiming "exits" would suppress
/// the diagnostic on a real fall-through path.
///
/// A match covers everything when:
/// 1. Every arm body always exits (necessary in either case), AND
/// 2. Either (a) at least one arm is a guardless catch-all (`case _:`
///    or a guardless capture like `case other:`), OR (b) the subject is
///    a sealed union and every variant is matched by a guardless arm.
fn match_arms_always_exit_aware(c: &Checker, m: &ruff_python_ast::StmtMatch) -> bool {
    if m.cases.is_empty() {
        return false;
    }
    if !m
        .cases
        .iter()
        .all(|case| body_always_exits_aware(c, &case.body))
    {
        return false;
    }
    match_cases_cover_subject(c, m)
}

/// Checker-aware variant of [`body_always_exits`]. Identical to the
/// structural form except that [`Stmt::Match`] dispatches to
/// [`match_arms_always_exit_aware`] so sealed-union exhaustiveness is
/// respected and open-typed matches without a catch-all correctly
/// report as falling-through.
fn body_always_exits_aware(c: &Checker, stmts: &[Stmt]) -> bool {
    stmts.last().is_some_and(|s| stmt_always_exits_aware(c, s))
}

fn stmt_always_exits_aware(c: &Checker, stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Return(_) | Stmt::Raise(_) | Stmt::Break(_) | Stmt::Continue(_) => true,
        Stmt::If(s) => {
            // `if True:` (constant-True test) is unconditional flow —
            // the body always runs, so it exits iff the body exits. The
            // `unsafe:` preprocessor lowers to exactly this shape, so
            // teaching the analysis about it lets `unsafe: ... return`
            // count as a definite return (F2).
            if is_constant_true(&s.test) {
                return body_always_exits_aware(c, &s.body);
            }
            body_always_exits_aware(c, &s.body)
                && elif_else_chain_always_exits_aware(c, &s.elif_else_clauses)
        }
        Stmt::Match(m) => match_arms_always_exit_aware(c, m),
        // A `with` / `async with` block exits the enclosing function
        // only when every terminal in its body is *non-suppressible*
        // — `return` / `break` / `continue`. Bare `raise` is
        // suppressible by the context manager's `__exit__`
        // (e.g. `contextlib.suppress(Exception)`), so a `with: raise`
        // is not a definite function exit. F3 examples end the with
        // body in `return`, which is non-suppressible.
        Stmt::With(w) => body_exits_non_suppressible_aware(c, &w.body),
        Stmt::Try(t) => {
            let finally_exits = !t.finalbody.is_empty() && body_always_exits_aware(c, &t.finalbody);
            let try_and_handlers_exit = body_always_exits_aware(c, &t.body)
                && t.handlers.iter().all(|h| {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    body_always_exits_aware(c, &h.body)
                })
                && (t.orelse.is_empty() || body_always_exits_aware(c, &t.orelse));
            finally_exits || try_and_handlers_exit
        }
        _ => false,
    }
}

/// True when `expr` is a literal `True`. Used to recognise the
/// `if True:` shape that the `unsafe:` preprocessor lowers to, so the
/// missing-return analysis can treat it as unconditional flow.
fn is_constant_true(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::BooleanLiteral(lit) if lit.value
    )
}

/// True when `stmts` always exits the enclosing function via a
/// *non-suppressible* terminal — `return` / `break` / `continue` (or a
/// composite whose every branch satisfies the same predicate).
///
/// Used for `with` / `async with` bodies in the missing-return
/// analysis: a context manager's `__exit__` can swallow `raise`
/// (`contextlib.suppress(Exception)`, custom managers returning
/// truthy from `__exit__`), so a body whose only terminal is `raise`
/// is not a definite function exit even though it locally exits the
/// statement. `return` / `break` / `continue` are not exceptions and
/// cannot be suppressed by `__exit__`.
fn body_exits_non_suppressible(stmts: &[Stmt]) -> bool {
    stmts.last().is_some_and(stmt_exits_non_suppressible)
}

fn stmt_exits_non_suppressible(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Return(_) | Stmt::Break(_) | Stmt::Continue(_) => true,
        // `raise` IS suppressible by the enclosing context manager.
        Stmt::Raise(_) => false,
        Stmt::If(s) => {
            if is_constant_true(&s.test) {
                return body_exits_non_suppressible(&s.body);
            }
            body_exits_non_suppressible(&s.body)
                && elif_else_chain_exits_non_suppressible(&s.elif_else_clauses)
        }
        Stmt::With(w) => body_exits_non_suppressible(&w.body),
        Stmt::Match(m) => {
            !m.cases.is_empty() && m.cases.iter().all(|c| body_exits_non_suppressible(&c.body))
        }
        Stmt::Try(t) => {
            let finally_exits =
                !t.finalbody.is_empty() && body_exits_non_suppressible(&t.finalbody);
            let try_and_handlers_exit = body_exits_non_suppressible(&t.body)
                && t.handlers.iter().all(|h| {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    body_exits_non_suppressible(&h.body)
                })
                && (t.orelse.is_empty() || body_exits_non_suppressible(&t.orelse));
            finally_exits || try_and_handlers_exit
        }
        _ => false,
    }
}

fn elif_else_chain_exits_non_suppressible(clauses: &[ruff_python_ast::ElifElseClause]) -> bool {
    let has_terminal_else = clauses.iter().any(|c| c.test.is_none());
    if !has_terminal_else {
        return false;
    }
    clauses.iter().all(|c| body_exits_non_suppressible(&c.body))
}

/// Checker-aware mirror of [`body_exits_non_suppressible`]. Identical
/// shape, but recurses into [`Stmt::Match`] through the exhaustiveness
/// pass so a sealed-union match whose arms each end in
/// `return`/`break`/`continue` counts as a non-suppressible exit.
fn body_exits_non_suppressible_aware(c: &Checker, stmts: &[Stmt]) -> bool {
    stmts
        .last()
        .is_some_and(|s| stmt_exits_non_suppressible_aware(c, s))
}

fn stmt_exits_non_suppressible_aware(c: &Checker, stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Return(_) | Stmt::Break(_) | Stmt::Continue(_) => true,
        Stmt::Raise(_) => false,
        Stmt::If(s) => {
            if is_constant_true(&s.test) {
                return body_exits_non_suppressible_aware(c, &s.body);
            }
            body_exits_non_suppressible_aware(c, &s.body)
                && elif_else_chain_exits_non_suppressible_aware(c, &s.elif_else_clauses)
        }
        Stmt::With(w) => body_exits_non_suppressible_aware(c, &w.body),
        Stmt::Match(m) => match_arms_exit_non_suppressible_aware(c, m),
        Stmt::Try(t) => {
            let finally_exits =
                !t.finalbody.is_empty() && body_exits_non_suppressible_aware(c, &t.finalbody);
            let try_and_handlers_exit = body_exits_non_suppressible_aware(c, &t.body)
                && t.handlers.iter().all(|h| {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    body_exits_non_suppressible_aware(c, &h.body)
                })
                && (t.orelse.is_empty() || body_exits_non_suppressible_aware(c, &t.orelse));
            finally_exits || try_and_handlers_exit
        }
        _ => false,
    }
}

fn elif_else_chain_exits_non_suppressible_aware(
    c: &Checker,
    clauses: &[ruff_python_ast::ElifElseClause],
) -> bool {
    let has_terminal_else = clauses.iter().any(|c| c.test.is_none());
    if !has_terminal_else {
        return false;
    }
    clauses
        .iter()
        .all(|cl| body_exits_non_suppressible_aware(c, &cl.body))
}

fn match_arms_exit_non_suppressible_aware(c: &Checker, m: &ruff_python_ast::StmtMatch) -> bool {
    // Mirror of `match_arms_always_exit_aware` but requiring every
    // arm body to exit via a non-suppressible terminal.
    if m.cases.is_empty() {
        return false;
    }
    if !m
        .cases
        .iter()
        .all(|case| body_exits_non_suppressible_aware(c, &case.body))
    {
        return false;
    }
    match_cases_cover_subject(c, m)
}

/// True when the unguarded arms of `m` collectively cover every value of the
/// subject's static type. Used by both `match_arms_always_exit_aware` and
/// `match_arms_exit_non_suppressible_aware` so the same recognition logic
/// drives the missing-return analysis for normal and `with`-body matches.
///
/// Covers:
/// - A guardless catch-all arm (`case _:` / `case x:` / `case <wild> as x:`).
/// - Sealed-union subjects where every variant is matched.
/// - Single-class subjects matched by a class-wildcard arm — including
///   `case C():`, `case C(field=name, ...):` with every field bound to a
///   capture, and `case C() as x:` / nested `MatchOr` of the same shape.
/// - `list` / `tuple` subjects covered by a `case [*xs]:` arm.
/// - Aliased union subjects (e.g. `type IntTree = int | list[IntTree]`)
///   where every variant of the unwrapped union is covered by an unguarded
///   arm that is a class-wildcard for that variant.
fn match_cases_cover_subject(c: &Checker, m: &ruff_python_ast::StmtMatch) -> bool {
    if m.cases.iter().any(|case| {
        case.guard.is_none()
            && matches!(
                &case.pattern,
                ruff_python_ast::Pattern::MatchAs(p) if p.pattern.is_none()
            )
    }) {
        return true;
    }
    let Some(subject_type) = match_subject_type(c, m) else {
        return false;
    };
    let unwrapped = c.unwrap_alias(&subject_type);
    cases_cover_type(c, &m.cases, &unwrapped)
}

fn match_subject_type(c: &Checker, m: &ruff_python_ast::StmtMatch) -> Option<Type> {
    if let Expr::Name(n) = m.subject.as_ref() {
        if let Some(binding) = c.env.lookup(n.id.as_str()) {
            return Some(binding.declared.clone());
        }
    }
    None
}

/// Return `true` iff the unguarded patterns in `cases` cover every inhabitant
/// of `ty`. Recurses on union variants so an aliased `int | list[...]` is
/// satisfied by arms covering each side individually.
fn cases_cover_type(c: &Checker, cases: &[MatchCase], ty: &Type) -> bool {
    if let Type::Union(variants) = ty {
        return variants.iter().all(|v| cases_cover_type(c, cases, v));
    }
    let class_name = match ty {
        Type::Class(n) => n.clone(),
        Type::Generic(head, _) => {
            if head == "Result" {
                let variants = ["Ok".to_string(), "Err".to_string()];
                let mut covered: HashSet<String> = HashSet::new();
                for case in cases {
                    if case.guard.is_some() {
                        continue;
                    }
                    collect_matched_class_names(&case.pattern, &mut covered);
                }
                return variants.iter().all(|v| covered.contains(v));
            }
            head.clone()
        }
        Type::Int => "int".to_string(),
        Type::Str => "str".to_string(),
        Type::Bool => "bool".to_string(),
        Type::Float => "float".to_string(),
        Type::Bytes => "bytes".to_string(),
        Type::None => "NoneType".to_string(),
        _ => return false,
    };
    if let Some(variants) = c.sealed_unions.get(class_name.as_str()).cloned() {
        let mut covered: HashSet<String> = HashSet::new();
        for case in cases {
            if case.guard.is_some() {
                continue;
            }
            if pattern_covers_class(c, &case.pattern, &class_name) {
                return true;
            }
            collect_matched_class_names(&case.pattern, &mut covered);
        }
        return variants.iter().all(|v| covered.contains(v));
    }
    if cases
        .iter()
        .any(|case| case.guard.is_none() && pattern_covers_class(c, &case.pattern, &class_name))
    {
        return true;
    }
    if class_name == "list" || class_name == "tuple" {
        sequence_cases_cover_all_lengths(cases)
    } else {
        false
    }
}

/// True when the unguarded sequence patterns in `cases` collectively cover
/// every possible length of a list/tuple subject. Recognises:
/// - `case [a, b, c]:` — fixed-length N coverage (matches length 3 only).
/// - `case [first, *rest]:` — tail-star coverage of length ≥ N where N is
///   the count of fixed elements before the star.
///
/// A coverage is total when there is a tail-star arm of minimum length N
/// and every shorter length 0..N is matched by a fixed-length arm.
fn sequence_cases_cover_all_lengths(cases: &[MatchCase]) -> bool {
    let mut exact: HashSet<usize> = HashSet::new();
    let mut tail_star_min: Option<usize> = None;
    for case in cases {
        if case.guard.is_some() {
            continue;
        }
        let Pattern::MatchSequence(seq) = &case.pattern else {
            continue;
        };
        let star_count = seq
            .patterns
            .iter()
            .filter(|p| matches!(p, Pattern::MatchStar(_)))
            .count();
        if star_count == 0 {
            if seq.patterns.iter().all(is_capture_or_underscore) {
                exact.insert(seq.patterns.len());
            }
            continue;
        }
        if star_count != 1 {
            continue;
        }
        if !matches!(seq.patterns.last(), Some(Pattern::MatchStar(_))) {
            continue;
        }
        if !seq.patterns[..seq.patterns.len() - 1]
            .iter()
            .all(is_capture_or_underscore)
        {
            continue;
        }
        let min_len = seq.patterns.len() - 1;
        tail_star_min = Some(match tail_star_min {
            Some(prev) => prev.min(min_len),
            None => min_len,
        });
    }
    let Some(min) = tail_star_min else {
        return false;
    };
    (0..min).all(|n| exact.contains(&n))
}

/// True if `pattern` (in an unguarded arm) matches every instance of
/// `class_name`. Recognises:
/// - `case _:` / `case x:` (always a wildcard for anything).
/// - `case <wild> as name:` (recurses on the inner pattern).
/// - `case C():` with no sub-patterns.
/// - `case C(field=p1, ...):` where every field of `C` is bound and every
///   sub-pattern `p_i` is itself a wildcard.
/// - `case [*xs]:` for `class_name == "list"` / `"tuple"`.
/// - `case <wild1> | <wild2> | ...:` where any branch matches.
fn pattern_covers_class(c: &Checker, pattern: &Pattern, class_name: &str) -> bool {
    match pattern {
        Pattern::MatchAs(a) => match &a.pattern {
            None => true,
            Some(inner) => pattern_covers_class(c, inner, class_name),
        },
        Pattern::MatchOr(o) => o
            .patterns
            .iter()
            .any(|p| pattern_covers_class(c, p, class_name)),
        Pattern::MatchClass(mc) => {
            let Expr::Name(n) = mc.cls.as_ref() else {
                return false;
            };
            if n.id.as_str() != class_name {
                return false;
            }
            if mc.arguments.patterns.is_empty() && mc.arguments.keywords.is_empty() {
                return true;
            }
            let Some(shape) = c.class_shapes.get(class_name) else {
                return false;
            };
            // Positional class pattern — `case Leaf(value):` /
            // `case Branch(left, right):`. Counts as a total match for
            // `class_name` when every supplied subpattern is a capture
            // (or wildcard) AND there are no keyword args adding
            // further filters. A pattern shorter than the declared
            // field count is still total: the omitted positionals are
            // unconstrained, the parser already accepts that shape, and
            // the runtime `match` will still select this arm for every
            // instance of the class (FINDINGS E6 + copilot review on
            // PR #87).
            if !mc.arguments.patterns.is_empty() {
                if !mc.arguments.keywords.is_empty() {
                    return false;
                }
                if mc.arguments.patterns.len() > shape.field_order.len() {
                    return false;
                }
                return mc.arguments.patterns.iter().all(is_capture_or_underscore);
            }
            // Keyword-only class pattern — every field must be bound by
            // a pattern that is itself a capture / wildcard.
            if mc.arguments.keywords.len() != shape.fields.len() {
                return false;
            }
            let bound: HashSet<&str> = mc
                .arguments
                .keywords
                .iter()
                .map(|kw| kw.attr.as_str())
                .collect();
            if !shape.fields.keys().all(|f| bound.contains(f.as_str())) {
                return false;
            }
            mc.arguments
                .keywords
                .iter()
                .all(|kw| is_capture_or_underscore(&kw.pattern))
        }
        Pattern::MatchSequence(seq) => {
            (class_name == "list" || class_name == "tuple")
                && seq.patterns.len() == 1
                && matches!(&seq.patterns[0], Pattern::MatchStar(_))
        }
        _ => false,
    }
}

/// True if `pattern` is a pure capture or wildcard with no nested filter:
/// `_`, `x`, or `<wild> as x` where the inner is itself such.
fn is_capture_or_underscore(pattern: &Pattern) -> bool {
    match pattern {
        Pattern::MatchAs(a) => match &a.pattern {
            None => true,
            Some(inner) => is_capture_or_underscore(inner),
        },
        _ => false,
    }
}

fn elif_else_chain_always_exits_aware(
    c: &Checker,
    clauses: &[ruff_python_ast::ElifElseClause],
) -> bool {
    let has_terminal_else = clauses.iter().any(|c| c.test.is_none());
    if !has_terminal_else {
        return false;
    }
    clauses
        .iter()
        .all(|cl| body_always_exits_aware(c, &cl.body))
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

/// Extract `R` from a `Generator[Y, S, R]` annotation so the
/// return-statement validator can check `return value` against the
/// declared return-payload type when the user has spelled out all
/// three parameters. Returns `None` for `Iterator[T]` / `Iterable[T]`
/// / async variants — those don't expose a return-type parameter and
/// `return value` payloads are accepted unchecked. Used by the
/// `c.in_generator` early-return in `check_stmt::Return` (FINDINGS
/// O6, refined per Codex review on PR #94).
fn extract_generator_return_type(typ: &Type) -> Option<Type> {
    if let Type::Generic(name, args) = typ {
        if name == "Generator" && args.len() == 3 {
            return Some(args[2].clone());
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

/// Type of a Python boolean operator expression (`a or b`, `a and b`).
///
/// Python's `or`/`and` are short-circuiting and return **the operand
/// value**, not a coerced `bool`. So `update.text or ""` evaluates to
/// `update.text` when truthy, otherwise `""` — and the static result
/// type is the union of the two operand types, with `None` stripped
/// from the LHS of an `or` (because that branch can only be taken when
/// the LHS is truthy, and `None` is always falsy).
///
/// We fold left-to-right for chains: `a or b or c` ≡ `(a or b) or c`.
///
/// The `and` rule is intentionally a conservative widening (`typeof(lhs)
/// ∪ typeof(rhs)`) rather than the strictest possible
/// `typeof(rhs)` ∪ "falsy types of lhs". Modelling Python's full
/// falsiness lattice (empty containers, `0`, `0.0`, `""`, etc.) is more
/// machinery than the typer needs today; the widened union still keeps
/// `let x: int = a and b` honest when both operands are `int`.
fn infer_bool_op(c: &mut Checker, b: &ruff_python_ast::ExprBoolOp) -> Type {
    if b.values.is_empty() {
        return Type::Bool;
    }
    let mut acc = infer_expr(c, &b.values[0]);
    for next in &b.values[1..] {
        let rhs = infer_expr(c, next);
        acc = match b.op {
            // `a or b` evaluates to `a` when `a` is truthy, otherwise `b`.
            // `truthy(&acc)` is `None` when `a` can never be truthy (e.g.
            // a bare `None` literal); in that case the result is just `rhs`.
            ruff_python_ast::BoolOp::Or => match truthy(&acc) {
                Some(t) => Type::union_of(vec![t, rhs]),
                None => rhs,
            },
            ruff_python_ast::BoolOp::And => Type::union_of(vec![acc, rhs]),
        };
    }
    acc
}

/// Return the type a value can have *when it is truthy*, or `None` if the
/// type has no truthy inhabitant. For a nullable `T | None` this strips the
/// `None` (because `None` is always falsy); for bare `Type::None` returns
/// `None` (the literal is unconditionally falsy, so the truthy branch is
/// impossible); for `Bool` we keep `Bool` (the truthy value is `True :
/// bool`); for everything else the type is unchanged (we do not try to
/// enumerate falsy literals like `0`, `""`, or empty containers — that
/// level of refinement is out of scope for the operator typer).
fn truthy(t: &Type) -> Option<Type> {
    match t {
        Type::None => None,
        Type::Bool => Some(Type::Bool),
        Type::Union(_) if t.is_nullable() => Some(t.strip_none()),
        other => Some(other.clone()),
    }
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
            let l_stripped = l.strip_none();
            let r_stripped = r.strip_none();
            if let Some(op_str) = arithmetic_op_str(b.op) {
                if !operator_operands_compatible(b.op, &l_stripped, &r_stripped) {
                    let span = (b.range.start().to_usize(), b.range.end().to_usize());
                    c.operator_type_mismatch(op_str, &l_stripped, &r_stripped, span);
                }
            }
            // Constant-fold safety lint: literal-zero RHS on `/`, `//`, `%`
            // is always a runtime `ZeroDivisionError`. Only the
            // literal-only case fires here so the check has zero false
            // positives — flow-sensitive analysis (`if d == 0` guards
            // narrowing the value) is out of scope. Skipped inside an
            // `unsafe:` region like every other diagnostic.
            if c.unsafe_depth == 0
                && matches!(b.op, Operator::Div | Operator::FloorDiv | Operator::Mod)
                && is_literal_zero(b.right.as_ref())
            {
                let span = (b.range.start().to_usize(), b.range.end().to_usize());
                let op_str = arithmetic_op_str(b.op).unwrap_or("/");
                c.diagnostics.push_error(TycError::div_by_zero_literal(
                    op_str,
                    &c.path,
                    c.source,
                    span.0,
                    span.1.saturating_sub(span.0).max(1),
                ));
            }
            // User-defined operator overloads on the LHS class take
            // precedence over the conservative numeric inference below.
            // Without this, `Vec2(...) * 5.0` would resolve to `Float`
            // via the `(_, Type::Float) => Type::Float` rule and the
            // `__mul__` return type recorded on `Vec2` would be ignored
            // (FINDINGS E5).
            //
            // The arg type IS checked against the dunder's first formal
            // — otherwise `v * "bad"` for
            // `def __mul__(self, scalar: float) -> Vec2` would silently
            // infer `Vec2` instead of emitting a type-mismatch (codex
            // review on PR #87). When a dunder exists on the LHS class
            // but its formal rejects the RHS, surface the operator
            // mismatch directly instead of falling through to the
            // permissive `Unknown` arm below — otherwise the bad call
            // would slip past as `let r: V = Unknown`.
            if let Some(dunder) = binop_dunder(b.op) {
                if let Type::Class(cls) | Type::Generic(cls, _) = &l_stripped {
                    if let Some(sig) = c.find_method(cls, dunder).cloned() {
                        if dunder_accepts(c, &sig, &r_stripped) {
                            return sig.return_type.clone();
                        }
                        if let Some(op_str) = arithmetic_op_str(b.op) {
                            let span = (b.range.start().to_usize(), b.range.end().to_usize());
                            c.operator_type_mismatch(op_str, &l_stripped, &r_stripped, span);
                        }
                        return sig.return_type.clone();
                    }
                }
                // Right-side reflected dunder (`__radd__`, `__rmul__`, …)
                // when the LHS is a built-in/primitive and the RHS is a
                // user class — `5.0 * Vec2(...)`.
                if let Some(rdunder) = binop_rdunder(b.op) {
                    if let Type::Class(cls) | Type::Generic(cls, _) = &r_stripped {
                        if let Some(sig) = c.find_method(cls, rdunder).cloned() {
                            if dunder_accepts(c, &sig, &l_stripped) {
                                return sig.return_type.clone();
                            }
                            if let Some(op_str) = arithmetic_op_str(b.op) {
                                let span = (b.range.start().to_usize(), b.range.end().to_usize());
                                c.operator_type_mismatch(op_str, &l_stripped, &r_stripped, span);
                            }
                            return sig.return_type.clone();
                        }
                    }
                }
            }
            // Conservative numeric arithmetic inference.
            match (&l_stripped, &r_stripped) {
                _ if matches!(b.op, Operator::Div)
                    && is_numeric(&l_stripped)
                    && is_numeric(&r_stripped) =>
                {
                    // Python's `/` is always true division — the result
                    // is `float` regardless of operand types. Returning
                    // `Type::Float` lets `let i: int = a / b` flag via
                    // the existing assignability check.
                    Type::Float
                }
                (Type::Int, Type::Int) => Type::Int,
                (Type::Float, _) | (_, Type::Float) => Type::Float,
                (Type::Str, Type::Str) if matches!(b.op, Operator::Add) => Type::Str,
                _ => Type::Unknown,
            }
        }
        Expr::BoolOp(b) => infer_bool_op(c, b),
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
            // Audit: passing a bypass-constructed instance into a
            // call is an escape — the callee may use the missing
            // fields. Then observe the call for forms that should
            // drop bypass tracking from the audit (`setattr`,
            // `c.method(...)`). The order matters: check the args
            // FIRST (before observe_call clears tracking) so
            // `setattr(c, ...)` is reported when `c` is partially
            // initialised. Skip arg-escape for setattr itself since
            // setattr's whole point is to assign fields.
            let is_setattr = matches!(
                call.func.as_ref(),
                Expr::Name(n) if n.id.as_str() == "setattr"
            );
            if !is_setattr {
                for arg in pos_args.iter() {
                    audit_check_escape(c, arg);
                }
                for kw in kw_args.iter() {
                    audit_check_escape(c, &kw.value);
                }
            }
            audit_observe_call(c, call);
            // Ok(value) and Err(error) are Result constructors: infer their
            // types as Generic("Ok", [T]) / Generic("Err", [E]) so that the
            // Result assignability rule in `assignable` can fire.
            if let Expr::Name(fn_name) = call.func.as_ref() {
                let ctor = fn_name.id.as_str();
                if (ctor == "Ok" || ctor == "Err") && pos_args.len() == 1 && kw_args.is_empty() {
                    let arg_type = infer_expr(c, &pos_args[0]);
                    return Type::Generic(ctor.to_owned(), vec![arg_type]);
                }
                // `Email("alice@example.com")` — newtype constructor
                // call. Type-check the single positional argument
                // against the declared base type, then return
                // `Type::Class(name)` so the caller sees the nominal
                // value, not the underlying primitive.
                if pos_args.len() == 1
                    && kw_args.is_empty()
                    && c.newtypes.contains_key(fn_name.id.as_str())
                {
                    let name = fn_name.id.as_str().to_owned();
                    let base = c.newtypes.get(&name).cloned().unwrap_or(Type::Unknown);
                    let arg_ty = infer_expr(c, &pos_args[0]);
                    if !c.is_assignable(&base, &arg_ty) {
                        let span = (
                            pos_args[0].range().start().to_usize(),
                            pos_args[0].range().end().to_usize(),
                        );
                        c.diagnostics.push_error(TycError::newtype_violation(
                            &name,
                            &base.display(),
                            &arg_ty.display(),
                            &c.path,
                            c.source,
                            span.0,
                            span.1.saturating_sub(span.0).max(1),
                        ));
                    }
                    return Type::Class(name);
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

            // Phase E: blocking-in-async call detection. When the
            // call sits directly inside an `async def` body and its
            // callee resolves to a known-blocking stdlib path, fire
            // `tyc::blocking_in_async` so the user wraps it in
            // `await asyncio.to_thread(...)` (or `run_in_executor`)
            // instead. The dotted-callee match excludes wrapper forms
            // because `asyncio.to_thread(time.sleep, 1)` itself is
            // `asyncio.to_thread(...)` — not in the registry.
            if c.in_async_function && c.unsafe_depth == 0 {
                if let Some(callee_path) = dotted_name_of(&call.func) {
                    if BLOCKING_CALLEES.iter().any(|p| *p == callee_path) {
                        let span = (call.range.start().to_usize(), call.range.end().to_usize());
                        c.diagnostics.push_warning(TycError::blocking_in_async(
                            &callee_path,
                            &c.path,
                            c.source,
                            span.0,
                            span.1.saturating_sub(span.0).max(1),
                        ));
                    }
                }
            }
            let func_type_raw = infer_expr(c, &call.func);
            // Unwrap transparent type aliases (`type Handler = Callable[..., R]`)
            // so that calls through the alias resolve to the underlying
            // `Type::Function` rather than the alias name. Without this,
            // the call-site match would land in the `Type::Class(...)`
            // (constructor) arm or `not_callable` and we'd lose both the
            // arity check and the return type. FINDINGS E4.
            let func_type = c.unwrap_alias(&func_type_raw);
            let call_span = (call.range.start().to_usize(), call.range.end().to_usize());

            // `tyc::missing_await` (FINDINGS #49): a *sync* function
            // calling an `async def` without `await` returns a
            // coroutine, not the declared return type — Python emits
            // "coroutine was never awaited" at runtime. The check is
            // gated on `in_sync_function` so `async def` bodies (which
            // use `await` naturally) and module scope are exempt.
            //
            // The `inside_await` counter (bumped by the `Expr::Await`
            // arm) suppresses the check when the call is the operand
            // of `await`. The arg-walk below additionally bumps
            // `inside_await` for arguments to known coroutine-accepting
            // functions like `asyncio.run(coro())`,
            // `asyncio.create_task(coro())`, `asyncio.gather(...)`,
            // `asyncio.ensure_future(...)`, `asyncio.wait(...)`,
            // `asyncio.wait_for(...)` — passing a bare coroutine to
            // any of these is the canonical entry-point pattern.
            if c.in_sync_function && c.inside_await == 0 {
                if let Expr::Name(n) = call.func.as_ref() {
                    if c.async_functions.contains(n.id.as_str()) {
                        let span = (n.range.start().to_usize(), n.range.end().to_usize());
                        c.missing_await(n.id.as_str(), span);
                    }
                }
            }
            let suppresses_missing_await = call_targets_coro_acceptor(call);

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
                        Expr::Attribute(a) => {
                            // For `module.fn(...)` callees (bare-import
                            // dotted access) the arity was stashed
                            // under the qualified `module.attr` key to
                            // avoid collisions between modules that
                            // export the same name. Mirror that
                            // qualification here so the lookup hits
                            // the right entry. The plain attribute
                            // name still covers method-call /
                            // instance-attribute callees.
                            let recv = infer_expr_readonly(c, &a.value);
                            match recv {
                                Type::Module(m) => Some(format!("{}.{}", m, a.attr.as_str())),
                                _ => Some(a.attr.as_str().to_owned()),
                            }
                        }
                        _ => None,
                    };
                    // Clone arity info so the mutable Checker borrows
                    // below (`infer_expr_ctx`, `c.mismatch`,
                    // `c.nullable_use`) don't fight an outstanding
                    // immutable borrow of `c.function_arity_info`.
                    //
                    // Method calls (`obj.foo(...)`) carry `ArityInfo` on
                    // the resolved `MethodSig` rather than in the
                    // module-level `function_arity_info` map, so we
                    // look there first whenever the call func is an
                    // attribute access on a known class instance. This
                    // closes the long-standing gap where method calls
                    // missing required args (e.g.
                    // `user.greet()` for `def greet(self, prefix: str)`)
                    // bypassed `tyc::arg_count`.
                    let method_arity_info: Option<ArityInfo> =
                        if let Expr::Attribute(a) = call.func.as_ref() {
                            method_arity_info_for_attribute(c, a)
                        } else {
                            None
                        };
                    let arity_info: Option<ArityInfo> = method_arity_info.or_else(|| {
                        fn_name
                            .as_deref()
                            .and_then(|n| c.function_arity_info.get(n))
                            .cloned()
                    });
                    let arity_outcome = if let Some(info) = arity_info.as_ref() {
                        check_arity_with_info(info, pos_args, kw_args)
                    } else {
                        // No name-keyed arity info — this is a method
                        // call, a callable-from-value, or a builtin
                        // generic method. We don't track defaults on
                        // methods today, so adopt a permissive shape:
                        // count kw args alongside positional, accept
                        // any total ≤ params.len() (defaults could
                        // cover the rest), and require ≥ params.len()
                        // when the function is variadic.
                        let total = pos_args.len() + kw_args.len();
                        let ok = if variadic {
                            total >= params.len()
                        } else {
                            total <= params.len()
                        };
                        if ok {
                            ArityCheck::Ok
                        } else {
                            ArityCheck::Other
                        }
                    };
                    match arity_outcome {
                        ArityCheck::Ok => {}
                        ArityCheck::UnknownKwarg {
                            name,
                            candidates,
                            span,
                        } => {
                            // FINDINGS #80: a typo'd kwarg surfaces a
                            // dedicated `tyc::unknown_kwarg` with the
                            // closest candidate, not an arg-count miscount.
                            let suggestion = suggest_candidate(&name, &candidates);
                            let fn_label = fn_name.clone().unwrap_or_else(|| "<call>".to_owned());
                            c.unknown_kwarg(&fn_label, &name, suggestion, span);
                        }
                        ArityCheck::Other => {
                            let name = fn_name.clone().unwrap_or_else(|| "<call>".to_owned());
                            // Prefer naming the missing parameter(s) when
                            // we can — the "expected N, got M" wording buries
                            // the actionable detail whenever the caller
                            // supplied some args by keyword but missed
                            // others. Falls back to `wrong_args` when no
                            // rich `ArityInfo` is available (callable
                            // values, method calls without a known sig).
                            let missing: Vec<String> = arity_info
                                .as_ref()
                                .map(|info| missing_required_params(info, pos_args, kw_args))
                                .unwrap_or_default();
                            if !missing.is_empty() {
                                c.missing_argument(&name, missing, call_span);
                            } else {
                                c.wrong_args(&name, params.len(), pos_args.len(), call_span);
                            }
                        }
                    }
                    // Argument type checks (per-pair, ignoring excess).
                    // Each argument is inferred with the corresponding
                    // formal as its expected type so empty-collection
                    // literals (`[]`, `{}`) and nested generic calls pick
                    // up the parameter's element types.
                    //
                    // FINDINGS #86: when the function has `*args`, only
                    // the first `positional_count` pos_args are paired
                    // with the structural `params` slice (which is
                    // `posonlyargs + args + kwonlyargs` flattened);
                    // beyond that, the remaining positional args are
                    // absorbed by `*args` and must be checked against
                    // its element type instead of the kw-only
                    // parameter that would otherwise occupy the slot.
                    let positional_cutoff = arity_info
                        .as_ref()
                        .map(|info| info.param_names.len())
                        .unwrap_or(params.len());
                    let vararg_elem_type = arity_info
                        .as_ref()
                        .and_then(|info| info.vararg_type.clone());
                    let mut actuals: Vec<Type> = Vec::with_capacity(params.len());
                    if suppresses_missing_await {
                        c.inside_await = c.inside_await.saturating_add(1);
                    }
                    for (i, arg) in pos_args.iter().enumerate() {
                        // Pick the right expected type:
                        // - First `positional_cutoff` pos_args fill the
                        //   declared positional parameters in order.
                        // - Beyond that, fall to the vararg element
                        //   type (if any). With no vararg we just stop
                        //   — the arity check has already flagged the
                        //   extra positional.
                        let expected: Type = if i < positional_cutoff && i < params.len() {
                            params[i].clone()
                        } else if let Some(t) = vararg_elem_type.clone() {
                            t
                        } else {
                            break;
                        };
                        let actual = infer_expr_ctx(c, arg, Some(&expected));
                        // Check the nullable-use case first: when the actual
                        // is nullable and the parameter is not, `nullable_use`
                        // is the more helpful diagnostic — it points at the
                        // narrowing fix (`if x is not None:` / `guard`).
                        // Emitting `type_mismatch` alongside would just be
                        // noise on the same span (FINDINGS #8). Only emit the
                        // type_mismatch when nullable_use isn't going to fire.
                        //
                        // Unbound PEP 695 TypeVars are exempt (FINDINGS #69):
                        // a `def f[T](x: T)` formal can absorb a `None`
                        // actual at the call site; bidirectional inference
                        // will bind `T = None` from the expected return.
                        // Transparent type aliases are also exempt — a
                        // `type JSON = int | str | None | ...` annotation
                        // is nullable once unwrapped even though the bare
                        // `Class("JSON")` doesn't look like it. FINDINGS #121.
                        let expected_unwrapped_nullable = c.unwrap_alias(&expected).is_nullable();
                        let nullable_into_non_nullable = !expected_unwrapped_nullable
                            && actual.is_nullable()
                            && !matches!(expected, Type::TypeVar(_));
                        if nullable_into_non_nullable {
                            if let Expr::Name(n) = arg {
                                let span = (
                                    n.range.start().to_usize(),
                                    n.range.start().to_usize() + n.id.as_str().len(),
                                );
                                c.nullable_use(n.id.as_str(), &expected, span);
                            } else {
                                // Non-name arg (e.g. `greet(find())`) — no
                                // identifier to point at, fall back to the
                                // generic mismatch diagnostic.
                                let span =
                                    (arg.range().start().to_usize(), arg.range().end().to_usize());
                                c.mismatch(&expected, &actual, span);
                            }
                        } else if !c.is_assignable(&expected, &actual) {
                            let span =
                                (arg.range().start().to_usize(), arg.range().end().to_usize());
                            c.mismatch(&expected, &actual, span);
                        }
                        actuals.push(actual);
                    }
                    // Keyword-argument type checks: when we have name-keyed
                    // `ArityInfo`, each kwarg can be paired with the
                    // parameter it fills (by position in `param_names`)
                    // and checked against that parameter's type. Without
                    // this, `def f(x: int) -> None; f(x="bad")` would
                    // pass `tyc check` once kwargs were counted in the
                    // arity check. (P1 review feedback from PR #57.)
                    //
                    // Clone the param-name slice up front so the
                    // mutable `infer_expr_ctx` / `c.mismatch` /
                    // `c.nullable_use` calls below don't fight the
                    // `&ArityInfo` immutable borrow that `arity_info`
                    // would keep alive across the loop.
                    let kwarg_param_names: Option<Vec<String>> =
                        arity_info.as_ref().map(|info| info.param_names.clone());
                    if let Some(param_names) = kwarg_param_names {
                        for kw in kw_args {
                            let Some(ident) = &kw.arg else { continue };
                            let Some(idx) = param_names.iter().position(|p| p == ident.as_str())
                            else {
                                continue;
                            };
                            if idx >= params.len() {
                                continue;
                            }
                            let actual = infer_expr_ctx(c, &kw.value, Some(&params[idx]));
                            let nullable_into_non_nullable =
                                !c.unwrap_alias(&params[idx]).is_nullable() && actual.is_nullable();
                            if nullable_into_non_nullable {
                                if let Expr::Name(n) = &kw.value {
                                    let span = (
                                        n.range.start().to_usize(),
                                        n.range.start().to_usize() + n.id.as_str().len(),
                                    );
                                    c.nullable_use(n.id.as_str(), &params[idx], span);
                                } else {
                                    let span = (
                                        kw.value.range().start().to_usize(),
                                        kw.value.range().end().to_usize(),
                                    );
                                    c.mismatch(&params[idx], &actual, span);
                                }
                            } else if !c.is_assignable(&params[idx], &actual) {
                                let span = (
                                    kw.value.range().start().to_usize(),
                                    kw.value.range().end().to_usize(),
                                );
                                c.mismatch(&params[idx], &actual, span);
                            }
                        }
                    }
                    if suppresses_missing_await {
                        c.inside_await = c.inside_await.saturating_sub(1);
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
                    let result = bind_typevars_and_substitute_bidirectional(
                        &params, &actuals, &ret, expected,
                    );
                    // FINDINGS #71: narrow `<dict[K, V]>.get(k, default)` to
                    // `V | type(default)`, which collapses to `V` when default
                    // is V-compatible. Without this, the one-arg signature
                    // (`V | None`) leaks into the two-arg call site even
                    // though Python guarantees a non-None return when a
                    // default is supplied. The default may be supplied
                    // positionally (`d.get("a", 0)`) or by keyword
                    // (`d.get("a", default=0)`); both forms are handled
                    // by looking for either shape before falling through
                    // to the nullable signature.
                    if let Expr::Attribute(attr) = call.func.as_ref() {
                        if attr.attr.as_str() == "get" {
                            let default_expr: Option<&Expr> = if pos_args.len() == 2 {
                                Some(&pos_args[1])
                            } else if pos_args.len() == 1 {
                                kw_args
                                    .iter()
                                    .find(|k| {
                                        k.arg
                                            .as_ref()
                                            .map(|ident| ident.as_str() == "default")
                                            .unwrap_or(false)
                                    })
                                    .map(|k| &k.value)
                            } else {
                                None
                            };
                            if let Some(default_expr) = default_expr {
                                let recv = infer_expr(c, &attr.value);
                                if let Type::Generic(head, dict_args) = &recv {
                                    if head == "dict" && dict_args.len() == 2 {
                                        let v = dict_args[1].clone();
                                        let default_ty = infer_expr(c, default_expr);
                                        return if c.is_assignable(&v, &default_ty) {
                                            v
                                        } else {
                                            Type::union_of(vec![v, default_ty])
                                        };
                                    }
                                }
                            }
                        }
                    }
                    result
                }
                Type::Class(name) => {
                    // Calling a class type whose shape we don't have
                    // (imported third-party class accessed through a
                    // field — `self.linear(x)` where
                    // `linear: torch.nn.Linear`) is most likely
                    // invoking the class's `__call__`, NOT constructing
                    // a new instance. We don't model `__call__` on
                    // foreign classes today, so degrade to `Unknown`
                    // and let the surrounding annotation drive
                    // assignability. The constructor arm below remains
                    // for project-local classes whose shape we know.
                    let is_project_class =
                        c.classes.iter().any(|n| n == &name) || c.class_shapes.contains_key(&name);
                    let func_is_class_name = matches!(
                        call.func.as_ref(),
                        Expr::Name(n) if c.classes.iter().any(|cn| cn == n.id.as_str())
                    );
                    if !is_project_class && !func_is_class_name {
                        for a in pos_args.iter() {
                            let _ = infer_expr(c, a);
                        }
                        for kw in kw_args.iter() {
                            let _ = infer_expr(c, &kw.value);
                        }
                        return Type::Unknown;
                    }
                    if let Some(shape) = c.class_shapes.get(&name).cloned() {
                        let candidates: Vec<String> = shape.fields.keys().cloned().collect();
                        for kw in kw_args {
                            let Some(ident) = &kw.arg else { continue };
                            let kw_name = ident.as_str();
                            if !shape.fields.contains_key(kw_name) {
                                let suggestion = suggest_candidate(kw_name, &candidates);
                                let span = (
                                    ident.range.start().to_usize(),
                                    ident.range.start().to_usize() + kw_name.len(),
                                );
                                c.unknown_kwarg(&name, kw_name, suggestion, span);
                            }
                        }
                        // Constructor-arity check: every non-defaulted field
                        // must be filled by a positional arg or matching kw.
                        // Reuses the same `check_arity_with_info` machinery
                        // that powers free-function arity, treating the
                        // auto-generated `__init__` as a function whose
                        // params are the class fields in declaration order.
                        // Without this check, calls like
                        // `ApiClient(base_url="x")` for a class with a
                        // required `api_key: str` field pass `tyc check`
                        // and only crash at runtime with
                        // `TypeError: missing 1 required positional argument`.
                        let info = class_constructor_arity(&shape);
                        if !info.param_names.is_empty() {
                            match check_arity_with_info(&info, pos_args, kw_args) {
                                ArityCheck::Ok => {}
                                ArityCheck::UnknownKwarg { .. } => {
                                    // Already handled by the dedicated
                                    // unknown-kwarg loop above.
                                }
                                ArityCheck::Other => {
                                    // Prefer the named-missing diagnostic
                                    // (`missing required argument: `client``)
                                    // when we can pinpoint which required
                                    // field(s) weren't filled — the call
                                    // already supplied some kwargs, so
                                    // "expected 1, got 4" buries the lede.
                                    // Falls back to `wrong_args` when the
                                    // arity violation isn't reducible to a
                                    // missing-name list (too many positionals,
                                    // positional/kwarg conflict on the same
                                    // parameter, etc.).
                                    let missing =
                                        missing_required_fields(&shape, pos_args, kw_args);
                                    if !missing.is_empty() {
                                        c.missing_argument(&name, missing, call_span);
                                    } else {
                                        let supplied = pos_args.len()
                                            + kw_args.iter().filter(|k| k.arg.is_some()).count();
                                        c.wrong_args(
                                            &name,
                                            info.min_positional,
                                            supplied,
                                            call_span,
                                        );
                                    }
                                }
                            }
                        }
                    }
                    if let Some(tparams) = c.class_type_params.get(&name).cloned() {
                        let mut bindings: HashMap<String, Type> = HashMap::new();
                        // Bidirectional: pin from the surrounding annotation.
                        // FINDINGS #68: when the expected type is a sealed
                        // union (or a type alias to one), look for a variant
                        // whose head matches the class being constructed and
                        // pin the type parameters from that variant. This
                        // makes `unwrap(Just(value=5))` for
                        // `unwrap(m: Maybe[int])` bind T=int from the
                        // `Just[int]` variant inside `Maybe[int]`.
                        let expected_unwrapped: Option<Type> = expected.map(|t| c.unwrap_alias(t));
                        let pinned_args: Option<Vec<Type>> = match expected_unwrapped.as_ref() {
                            Some(Type::Generic(exp_name, exp_args))
                                if exp_name == &name && exp_args.len() == tparams.len() =>
                            {
                                Some(exp_args.clone())
                            }
                            Some(Type::Union(variants)) => variants.iter().find_map(|v| {
                                let v = c.unwrap_alias(v);
                                match v {
                                    Type::Generic(exp_name, exp_args)
                                        if exp_name == name && exp_args.len() == tparams.len() =>
                                    {
                                        Some(exp_args)
                                    }
                                    _ => None,
                                }
                            }),
                            _ => None,
                        };
                        if let Some(args) = pinned_args {
                            for (tp, arg) in tparams.iter().zip(args.iter()) {
                                bindings.insert(tp.clone(), arg.clone());
                            }
                        }
                        // Forward: read each kwarg, match it to the
                        // class's field annotation, and if the field's
                        // type is a TypeVar, bind it from the arg's
                        // inferred type.
                        let class_shape = c.class_shapes.get(&name).cloned();
                        if let Some(shape) = class_shape {
                            for kw in kw_args {
                                if let Some(ident) = &kw.arg {
                                    if let Some(field_ty) =
                                        shape.fields.get(ident.as_str()).cloned()
                                    {
                                        let arg_ty = infer_expr(c, &kw.value);
                                        bind_field_typevars(&field_ty, &arg_ty, &mut bindings);
                                    }
                                }
                            }
                        }
                        let args: Vec<Type> = tparams
                            .iter()
                            .map(|tp| bindings.remove(tp).unwrap_or(Type::Unknown))
                            .collect();
                        Type::Generic(name, args)
                    } else {
                        Type::Class(name)
                    }
                }
                Type::Unknown | Type::Any => {
                    if suppresses_missing_await {
                        c.inside_await = c.inside_await.saturating_add(1);
                    }
                    for a in pos_args.iter() {
                        let _ = infer_expr(c, a);
                    }
                    if suppresses_missing_await {
                        c.inside_await = c.inside_await.saturating_sub(1);
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
            // Bare-import dotted access: when the receiver resolves
            // to `Type::Module(name)`, look the attribute up in the
            // project shape registry. A matching class returns
            // `Type::Class(<attr_name>)` so the constructor call
            // site flows through the arity check; a free function
            // returns the corresponding `Type::Function`. The
            // foreign class's shape is also lazily folded into
            // `class_shapes` so the constructor call's
            // `Type::Class(name)` arm can find it.
            if let Type::Module(mod_name) = &recv {
                // Bump the Arc instead of cloning the whole registry
                // so the cost stays O(1) regardless of project size.
                let registry = std::sync::Arc::clone(&c.module_registry);
                if let Some(shapes) = registry.get(mod_name) {
                    // Qualify the stashed name with the module so two
                    // imports that both expose `Client` don't collide
                    // in `class_shapes` / `function_arity_info` — the
                    // first-seen shape would otherwise win for both
                    // call sites. The qualified name is also what
                    // the user sees in the diagnostic
                    // ("`clients.ApiClient`"), which is more
                    // informative than the unqualified form.
                    // FINDINGS — gemini + codex review of v0.2.0.
                    let qualified = format!("{}.{}", mod_name, attr_name);
                    if let Some(shape) = shapes.class_shapes.get(attr_name) {
                        let shape = shape.clone();
                        c.class_shapes.entry(qualified.clone()).or_insert(shape);
                        if let Some(tps) = shapes.class_type_params.get(attr_name) {
                            let tps = tps.clone();
                            c.class_type_params.entry(qualified.clone()).or_insert(tps);
                        }
                        return Type::Class(qualified);
                    }
                    if let Some(info) = shapes.function_arities.get(attr_name) {
                        let info = info.clone();
                        let params = vec![Type::Unknown; info.param_names.len()];
                        let variadic = info.max_positional.is_none();
                        c.function_arity_info.entry(qualified).or_insert(info);
                        return Type::Function {
                            params,
                            ret: Box::new(Type::Unknown),
                            variadic,
                        };
                    }
                }
                // Attribute not found in the registry — degrade to
                // `Unknown` rather than emitting a diagnostic. The
                // registry intentionally omits private names; this
                // mirrors how an unannotated Python module attribute
                // would land in `unsafe:`-style territory.
                return Type::Unknown;
            }
            match &recv {
                Type::Class(class_name) => {
                    let class_name = class_name.clone();
                    let receiver_is_class_name = matches!(
                        a.value.as_ref(),
                        Expr::Name(n) if c.classes.iter().any(|cn| cn == n.id.as_str())
                    );
                    if let Some(sig) = c.find_method(class_name.as_str(), attr_name) {
                        // `@property` methods are read as attributes — return
                        // the underlying type directly instead of a bound-
                        // method handle. FINDINGS #63.
                        if sig.is_property {
                            return sig.return_type.clone();
                        }
                        let mut arity = sig.arity;
                        let mut params = sig.param_types.clone();
                        if receiver_is_class_name && !sig.is_static && !sig.is_classmethod {
                            arity = arity.saturating_add(1);
                            params.insert(0, Type::Class(class_name.clone()));
                        }
                        // Pad with Unknowns if the recorded param_types is
                        // shorter than the recorded arity (defensive — both
                        // should be derived from the same source).
                        if params.len() < arity {
                            params.resize(arity, Type::Unknown);
                        }
                        let ret = sig.return_type.clone();
                        return Type::Function {
                            params,
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
                            if sig.is_property {
                                return sig.return_type.clone();
                            }
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
            let value_ty = infer_expr(c, &s.value);
            let _ = infer_expr(c, &s.slice);
            if let Type::Generic(head, elts) = &value_ty {
                if head == "tuple" && !elts.is_empty() {
                    if let Some(idx) = const_int_index(&s.slice) {
                        let arity = elts.len();
                        let arity_i = arity as i64;
                        let resolved = if idx >= 0 && idx < arity_i {
                            Some(idx as usize)
                        } else if idx < 0 && idx >= -arity_i {
                            Some((arity_i + idx) as usize)
                        } else {
                            None
                        };
                        match resolved {
                            Some(i) => return elts[i].clone(),
                            None => {
                                let span = (s.range.start().to_usize(), s.range.end().to_usize());
                                c.tuple_index_out_of_range(arity, idx, span);
                                return Type::Unknown;
                            }
                        }
                    }
                }
            }
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
            // Homogeneous variadic tuple expected: every slot inherits
            // the same element type. Lets `let xs: tuple[float, ...] =
            // (1, 2, 3)` widen the int literals to float at inference,
            // and propagates through nested generic calls the same way
            // a fixed-arity tuple expectation does.
            let variadic_elem: Option<&Type> = match expected {
                Some(Type::Generic(h, a)) if h == "tuple_variadic" && a.len() == 1 => Some(&a[0]),
                _ => None,
            };
            let elts: Vec<Type> = t
                .elts
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let hint = per_slot.map(|a| &a[i]).or(variadic_elem);
                    infer_expr_ctx(c, e, hint)
                })
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
        // `await EXPR` — bump the `inside_await` counter while inferring
        // EXPR so a `Call` to an async function inside it does not
        // trip the `tyc::missing_await` check (FINDINGS #49). The
        // inferred type is the inner call's return type (`async def f()
        // -> int` → `await f()` is `int`).
        Expr::Await(a) => {
            c.inside_await = c.inside_await.saturating_add(1);
            let inner = infer_expr_ctx(c, &a.value, expected);
            c.inside_await = c.inside_await.saturating_sub(1);
            inner
        }
        _ => Type::Unknown,
    }
}

/// Operators whose operand-type compatibility is checked by
/// `operator_operands_compatible`. Returns the operator's Python source
/// form for diagnostic text, or `None` for ops we don't yet check
/// (bitwise / shifts, MatMult).
/// Does the dunder method's first declared formal accept `arg_type`?
/// Used before adopting a dunder's return type as the operator's
/// result type — without it, `v * "bad"` for `def __mul__(self,
/// scalar: float) -> Vec2` would silently infer `Vec2` instead of
/// surfacing the type mismatch. A dunder with no recorded param type
/// (older shape / unannotated formal) is treated as permissive.
fn dunder_accepts(c: &Checker, sig: &MethodSig, arg_type: &Type) -> bool {
    let Some(first) = sig.param_types.first() else {
        return true;
    };
    if matches!(first, Type::Unknown | Type::Any) {
        return true;
    }
    c.is_assignable(first, arg_type)
}

/// Python dunder name for the binary operator `op`. Returned by
/// `BinOp` inference so a user class with `def __mul__(self, ...) -> R`
/// resolves the call to `R` rather than falling through to the
/// numeric-coercion table (FINDINGS E5).
fn binop_dunder(op: Operator) -> Option<&'static str> {
    match op {
        Operator::Add => Some("__add__"),
        Operator::Sub => Some("__sub__"),
        Operator::Mult => Some("__mul__"),
        Operator::MatMult => Some("__matmul__"),
        Operator::Div => Some("__truediv__"),
        Operator::FloorDiv => Some("__floordiv__"),
        Operator::Mod => Some("__mod__"),
        Operator::Pow => Some("__pow__"),
        Operator::LShift => Some("__lshift__"),
        Operator::RShift => Some("__rshift__"),
        Operator::BitAnd => Some("__and__"),
        Operator::BitOr => Some("__or__"),
        Operator::BitXor => Some("__xor__"),
    }
}

/// Reflected (right-side) dunder name, used when the LHS is a
/// primitive and the RHS is a user class.
fn binop_rdunder(op: Operator) -> Option<&'static str> {
    match op {
        Operator::Add => Some("__radd__"),
        Operator::Sub => Some("__rsub__"),
        Operator::Mult => Some("__rmul__"),
        Operator::MatMult => Some("__rmatmul__"),
        Operator::Div => Some("__rtruediv__"),
        Operator::FloorDiv => Some("__rfloordiv__"),
        Operator::Mod => Some("__rmod__"),
        Operator::Pow => Some("__rpow__"),
        Operator::LShift => Some("__rlshift__"),
        Operator::RShift => Some("__rrshift__"),
        Operator::BitAnd => Some("__rand__"),
        Operator::BitOr => Some("__ror__"),
        Operator::BitXor => Some("__rxor__"),
    }
}

fn arithmetic_op_str(op: Operator) -> Option<&'static str> {
    match op {
        Operator::Add => Some("+"),
        Operator::Sub => Some("-"),
        Operator::Mult => Some("*"),
        Operator::Div => Some("/"),
        Operator::FloorDiv => Some("//"),
        Operator::Mod => Some("%"),
        Operator::Pow => Some("**"),
        _ => None,
    }
}

/// True if either side is something we shouldn't flag at all — values
/// of `Any`/`Unknown`/`TypeVar`, a user-defined class (which might
/// implement `__add__` / `__mul__` / …), any composite type that
/// embeds one of those, or any union (we don't know which variant the
/// value actually holds at this point, and even all-primitive unions
/// like `int | str` may be valid for some variant).
fn operand_is_unflaggable(t: &Type) -> bool {
    match t {
        Type::Any | Type::Unknown | Type::TypeVar(_) => true,
        Type::Class(_) => true,
        Type::Function { .. } => true,
        Type::Union(_) => true,
        Type::Generic(_, args) => args.iter().any(operand_is_unflaggable),
        Type::Int | Type::Str | Type::Bool | Type::Float | Type::Bytes | Type::None => false,
        // Module references can't be operands of arithmetic / comparison.
        // Producing the diagnostic here would surprise users; downstream
        // arithmetic on a `Type::Module` value is already nonsense and
        // the existing type-mismatch check at the call site fires.
        Type::Module(_) => true,
    }
}

fn is_numeric(t: &Type) -> bool {
    matches!(t, Type::Int | Type::Float | Type::Bool)
}

/// Conservative compatibility check for the arithmetic / concat
/// operators in [`arithmetic_op_str`]. Returns `false` only for pairs
/// that are clearly wrong by Python's runtime semantics on built-in
/// types; anything involving an unknown or user class is treated as
/// possibly-compatible.
fn operator_operands_compatible(op: Operator, l: &Type, r: &Type) -> bool {
    if operand_is_unflaggable(l) || operand_is_unflaggable(r) {
        return true;
    }
    match op {
        Operator::Add => {
            if is_numeric(l) && is_numeric(r) {
                return true;
            }
            if matches!(l, Type::Str) && matches!(r, Type::Str) {
                return true;
            }
            if matches!(l, Type::Bytes) && matches!(r, Type::Bytes) {
                return true;
            }
            if let (Type::Generic(ln, _), Type::Generic(rn, _)) = (l, r) {
                if ln == rn && (ln == "list" || ln == "tuple") {
                    return true;
                }
            }
            false
        }
        Operator::Sub | Operator::Mod | Operator::Pow | Operator::FloorDiv | Operator::Div => {
            is_numeric(l) && is_numeric(r)
        }
        Operator::Mult => {
            if is_numeric(l) && is_numeric(r) {
                return true;
            }
            // Repetition: str/bytes/list/tuple * int (either order).
            let is_repeatable = |t: &Type| {
                matches!(t, Type::Str | Type::Bytes)
                    || matches!(t, Type::Generic(n, _) if n == "list" || n == "tuple")
            };
            (is_repeatable(l) && matches!(r, Type::Int | Type::Bool))
                || (is_repeatable(r) && matches!(l, Type::Int | Type::Bool))
        }
        _ => true,
    }
}

/// Recognise a literal zero on the RHS of a division-style operator.
/// Catches both `int` (`0`) and `float` (`0.0`, `-0.0`) literals, plus
/// the unary-minus form of either (`-0`, `-0.0`). Any non-literal
/// expression returns `false` — we intentionally don't do flow
/// analysis here; this is the constant-fold-only safety lint.
fn is_literal_zero(expr: &Expr) -> bool {
    match expr {
        Expr::NumberLiteral(n) => match &n.value {
            Number::Int(i) => i.as_i64() == Some(0),
            Number::Float(f) => *f == 0.0,
            _ => false,
        },
        Expr::UnaryOp(u) if matches!(u.op, ruff_python_ast::UnaryOp::USub) => {
            is_literal_zero(u.operand.as_ref())
        }
        _ => false,
    }
}

/// Extract a constant integer index from an expression used in
/// `Expr::Subscript`. Supports `Number::Int` literals and the unary
/// negation of one. Anything else returns `None`.
fn const_int_index(expr: &Expr) -> Option<i64> {
    match expr {
        Expr::NumberLiteral(n) => match &n.value {
            Number::Int(i) => i.as_i64(),
            _ => None,
        },
        Expr::UnaryOp(u) if matches!(u.op, ruff_python_ast::UnaryOp::USub) => {
            if let Expr::NumberLiteral(n) = u.operand.as_ref() {
                if let Number::Int(i) = &n.value {
                    return i.as_i64().and_then(|v| v.checked_neg());
                }
            }
            None
        }
        _ => None,
    }
}

// ── sealed union helpers ──────────────────────────────────────────────────────

/// Extract the list of variant class names from a `type Foo = A | B | C`
/// value expression.  Returns `None` if the expression is not a pure union of
/// bare names (meaning it is not a sealed union declaration we can track).
///
/// Uses an explicit stack rather than recursion to avoid stack overflow on
/// deeply nested union expressions (e.g. `A | B | C | ... | Z`).
/// If `assign` has the exact shape `Name = NewType("Name", Base)`, return
/// `(name, base_expr)`. Used to recognise the preprocessed form of
/// `newtype Name = Base` in the module body and register `Name` as a
/// nominal newtype.
///
/// The string literal in the first argument must match the LHS target
/// name exactly — any deviation rejects the pattern and falls through to
/// regular assignment handling.
fn extract_newtype_decl(assign: &StmtAssign) -> Option<(String, Expr)> {
    if assign.targets.len() != 1 {
        return None;
    }
    let target = match &assign.targets[0] {
        Expr::Name(n) => n,
        _ => return None,
    };
    let call = match assign.value.as_ref() {
        Expr::Call(c) => c,
        _ => return None,
    };
    let callee = match call.func.as_ref() {
        Expr::Name(n) => n,
        _ => return None,
    };
    if callee.id.as_str() != "NewType" {
        return None;
    }
    if call.arguments.args.len() != 2 || !call.arguments.keywords.is_empty() {
        return None;
    }
    let first = call.arguments.args.first()?;
    let s = match first {
        Expr::StringLiteral(s) => s,
        _ => return None,
    };
    if s.value.to_str() != target.id.as_str() {
        return None;
    }
    let base = call.arguments.args.get(1)?.clone();
    Some((target.id.as_str().to_owned(), base))
}

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
        for variant in variants {
            if pattern_covers_class(c, &case.pattern, variant) {
                covered.insert(variant.clone());
            }
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

/// Walk a `case` pattern and emit `tyc::unknown_kwarg` / `tyc::arg_count`
/// for `MatchClass` patterns that reference fields or positional slots the
/// class doesn't have (FINDINGS R3.8 / R3.12.f).
fn check_pattern_class_fields(c: &mut Checker, pattern: &Pattern) {
    match pattern {
        Pattern::MatchClass(mc) => {
            if let Expr::Name(cls_name) = mc.cls.as_ref() {
                let name = cls_name.id.as_str().to_owned();
                if let Some(shape) = c.class_shapes.get(&name).cloned() {
                    let candidates: Vec<String> = shape.fields.keys().cloned().collect();
                    for kw in &mc.arguments.keywords {
                        let kw_name = kw.attr.as_str();
                        if !shape.fields.contains_key(kw_name) {
                            let suggestion = suggest_candidate(kw_name, &candidates);
                            let span = (
                                kw.attr.range.start().to_usize(),
                                kw.attr.range.start().to_usize() + kw_name.len(),
                            );
                            c.unknown_kwarg(&name, kw_name, suggestion, span);
                        }
                    }
                    let pos = mc.arguments.patterns.len();
                    if pos > shape.fields.len() {
                        let span = (mc.range.start().to_usize(), mc.range.end().to_usize());
                        c.wrong_args(&name, shape.fields.len(), pos, span);
                    }
                }
            }
            for p in &mc.arguments.patterns {
                check_pattern_class_fields(c, p);
            }
            for kw in &mc.arguments.keywords {
                check_pattern_class_fields(c, &kw.pattern);
            }
        }
        Pattern::MatchAs(a) => {
            if let Some(inner) = &a.pattern {
                check_pattern_class_fields(c, inner);
            }
        }
        Pattern::MatchOr(o) => {
            for p in &o.patterns {
                check_pattern_class_fields(c, p);
            }
        }
        Pattern::MatchSequence(seq) => {
            for p in &seq.patterns {
                check_pattern_class_fields(c, p);
            }
        }
        Pattern::MatchMapping(m) => {
            for p in &m.patterns {
                check_pattern_class_fields(c, p);
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
        // 0.2.3: when we can pin down which parameter wasn't filled
        // (here `b`), the more actionable `missing_argument` diagnostic
        // fires in place of the count-based form. Either wording is
        // accepted to keep the test future-proof against further
        // diagnostic refinements.
        assert!(
            msg.contains("missing required argument") || msg.contains("wrong number of arguments"),
            "got {}",
            msg
        );
    }

    // ── constructor-arity checks ───────────────────────────────────────
    //
    // The generated `__init__` of a `class` / `model` declaration must
    // be called with every non-defaulted field filled — either
    // positionally or by keyword. Without these checks, a call like
    // `ApiClient(base_url="...")` for a class with a required
    // `api_key: str` field passed `tyc check` and only crashed at
    // runtime with `TypeError: missing 1 required positional argument`.

    #[test]
    fn ctor_missing_required_kwarg_errors() {
        let src = "\
class ApiClient:
    api_key: str
    base_url: str

let c: ApiClient = ApiClient(base_url=\"https://api.example.com\")
";
        let d = check(src);
        assert!(d.has_errors(), "missing required field must error");
        let msg = format!("{}", d.errors()[0]);
        // 0.2.3: when the call already supplied `base_url`, the
        // dedicated `missing_argument` diagnostic names the missing
        // `api_key` instead of the count-based form.
        assert!(
            msg.contains("ApiClient")
                && msg.contains("missing required argument")
                && msg.contains("api_key"),
            "got: {msg}"
        );
    }

    #[test]
    fn ctor_no_args_errors_when_required() {
        let src = "\
class ApiClient:
    api_key: str
    base_url: str

let c: ApiClient = ApiClient()
";
        let d = check(src);
        assert!(d.has_errors(), "bare `ApiClient()` must error");
    }

    #[test]
    fn ctor_all_required_filled_positionally_passes() {
        let src = "\
class ApiClient:
    api_key: str
    base_url: str

let c: ApiClient = ApiClient(\"k\", \"u\")
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn ctor_all_required_filled_by_kwarg_passes() {
        let src = "\
class ApiClient:
    api_key: str
    base_url: str

let c: ApiClient = ApiClient(api_key=\"k\", base_url=\"u\")
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn ctor_field_with_default_is_optional() {
        let src = "\
class ApiClient:
    api_key: str
    base_url: str = \"https://default.example.com\"

let c: ApiClient = ApiClient(api_key=\"k\")
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn ctor_nullable_field_still_required_when_no_default() {
        // Typhon does NOT auto-inject `= None` for `T?` fields, so the
        // emitted `@dataclass` still requires them at construction. The
        // check must match runtime — leniency here would let crashing
        // code pass the build.
        let src = "\
class Foo:
    name: str?

let f: Foo = Foo()
";
        let d = check(src);
        assert!(d.has_errors(), "`T?` without `= None` must still error");
    }

    #[test]
    fn ctor_nullable_field_with_explicit_none_default_is_optional() {
        let src = "\
class Foo:
    name: str? = None

let f: Foo = Foo()
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn ctor_model_class_arity_checked() {
        let src = "\
model User:
    id: int
    name: str

let u: User = User(id=1)
";
        let d = check(src);
        assert!(d.has_errors(), "model classes must have the same check");
    }

    #[test]
    fn ctor_too_many_positional_args_errors() {
        let src = "\
class Point:
    x: int
    y: int

let p: Point = Point(1, 2, 3)
";
        let d = check(src);
        assert!(d.has_errors(), "extra positional must error");
        let msg = format!("{}", d.errors()[0]);
        // The "missing_argument" path is wrong here — every required
        // field is filled positionally; the surplus arg is the real
        // bug. PR-#90 review: we must fall back to the count-based
        // `arg_count` diagnostic in this case.
        assert!(
            !msg.contains("missing required argument"),
            "too-many-positionals must not fire missing_argument: {msg}"
        );
        assert!(msg.contains("wrong number of arguments"), "got: {msg}");
    }

    #[test]
    fn ctor_positional_kwarg_conflict_falls_back_to_arg_count() {
        // `Point(1, x=2)` double-binds `x` (positionally + by kwarg);
        // `check_arity_with_info` returns `ArityCheck::Other`, but
        // suggesting "missing `y`" would send the user the wrong
        // fix. The named-missing diagnostic must defer to
        // `wrong_args` here. PR-#90 codex review.
        let src = "\
class Point:
    x: int
    y: int

let p: Point = Point(1, x=2)
";
        let d = check(src);
        assert!(d.has_errors(), "double-bound positional must error");
        let msg = format!("{}", d.errors()[0]);
        assert!(
            !msg.contains("missing required argument"),
            "conflict must not be reported as missing-field: {msg}"
        );
    }

    #[test]
    fn fn_positional_kwarg_conflict_falls_back_to_arg_count() {
        // Same shape as the ctor case but for a free function:
        // `f(1, a=2)` for `def f(a, b)`.
        let src = "\
def f(a: int, b: int) -> int:
    return a + b

let r: int = f(1, a=2)
";
        let d = check(src);
        assert!(d.has_errors(), "double-bound positional must error");
        let msg = format!("{}", d.errors()[0]);
        assert!(
            !msg.contains("missing required argument"),
            "conflict must not be reported as missing-arg: {msg}"
        );
    }

    #[test]
    fn fn_too_many_positionals_falls_back_to_arg_count() {
        // `def f(a, *, b)` with `f(1, 2)` — `b` is required but
        // the *real* error is the second positional arg, not a
        // missing `b`. PR-#90 codex review.
        let src = "\
def f(a: int, *, b: int) -> int:
    return a + b

let r: int = f(1, 2)
";
        let d = check(src);
        assert!(d.has_errors(), "too-many-positionals must error");
        let msg = format!("{}", d.errors()[0]);
        assert!(
            !msg.contains("missing required argument"),
            "too-many-positionals must not fire missing_argument: {msg}"
        );
    }

    // ── method-arity checks ─────────────────────────────────────────
    //
    // Method calls (`obj.foo(args)`) used to fall into the permissive
    // arity shape because `Expr::Attribute` callees had no name-keyed
    // arity info. They now carry full `ArityInfo` on `MethodSig`.

    #[test]
    fn method_missing_required_arg_errors() {
        let src = "\
class User:
    name: str

impl User:
    def greet(self, prefix: str) -> str:
        return prefix + self.name

let u: User = User(name=\"x\")
let g: str = u.greet()
";
        let d = check(src);
        assert!(
            d.has_errors(),
            "method call missing required arg must error"
        );
        let msg = format!("{}", d.errors()[0]);
        // 0.2.3: named-missing wording.
        assert!(
            msg.contains("greet")
                && msg.contains("missing required argument")
                && msg.contains("prefix"),
            "got: {msg}"
        );
    }

    #[test]
    fn method_call_with_required_arg_passes() {
        let src = "\
class User:
    name: str

impl User:
    def greet(self, prefix: str) -> str:
        return prefix + self.name

let u: User = User(name=\"x\")
let g: str = u.greet(\"hi \")
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn method_with_default_param_passes_without_arg() {
        let src = "\
class User:
    name: str

impl User:
    def greet(self, prefix: str = \"hi \") -> str:
        return prefix + self.name

let u: User = User(name=\"x\")
let g: str = u.greet()
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    // ── post-construction field-init audit ─────────────────────────
    //
    // The audit catches `X.__new__(X)` / `object.__new__(X)` bypass
    // patterns: the auto-generated `__init__` is skipped, so any
    // required field not assigned before the instance escapes
    // (return / call arg) would crash at runtime with
    // `AttributeError`. Conservative by design — drops tracking on
    // `setattr`, method calls, and inside `unsafe:` blocks.

    #[test]
    fn audit_bypass_construct_missing_field_on_return_errors() {
        let src = "\
class ApiClient:
    api_key: str
    base_url: str

def make() -> ApiClient:
    let c: ApiClient = ApiClient.__new__(ApiClient)
    c.base_url = \"x\"
    return c
";
        let d = check(src);
        assert!(d.has_errors(), "bypass with missing field must error");
        let msg = format!("{}", d.errors()[0]);
        assert!(
            msg.contains("api_key") && msg.contains("ApiClient"),
            "got: {msg}"
        );
    }

    #[test]
    fn audit_bypass_construct_all_fields_set_passes() {
        let src = "\
class ApiClient:
    api_key: str
    base_url: str

def make() -> ApiClient:
    let c: ApiClient = ApiClient.__new__(ApiClient)
    c.api_key = \"sk-x\"
    c.base_url = \"x\"
    return c
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn audit_bypass_escape_via_call_arg_errors() {
        let src = "\
class ApiClient:
    api_key: str
    base_url: str

def consume(c: ApiClient) -> None:
    pass

def f() -> None:
    let c: ApiClient = ApiClient.__new__(ApiClient)
    c.base_url = \"x\"
    consume(c)
";
        let d = check(src);
        assert!(d.has_errors(), "call-arg escape must error");
    }

    #[test]
    fn audit_object_new_form_also_tracked() {
        let src = "\
class ApiClient:
    api_key: str
    base_url: str

def make() -> ApiClient:
    let c: ApiClient = object.__new__(ApiClient)
    return c
";
        let d = check(src);
        assert!(d.has_errors(), "`object.__new__(X)` must also be tracked");
    }

    #[test]
    fn audit_unsafe_block_suppresses() {
        let src = "\
class ApiClient:
    api_key: str
    base_url: str

def make() -> ApiClient:
    unsafe:
        let c: ApiClient = ApiClient.__new__(ApiClient)
        return c
";
        let d = check(src);
        assert!(!d.has_errors(), "unsafe must suppress: {:?}", d.errors());
    }

    #[test]
    fn audit_setattr_drops_tracking() {
        let src = "\
class ApiClient:
    api_key: str
    base_url: str

def make() -> ApiClient:
    let c: ApiClient = ApiClient.__new__(ApiClient)
    setattr(c, \"api_key\", \"x\")
    setattr(c, \"base_url\", \"y\")
    return c
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "setattr must defeat audit: {:?}",
            d.errors()
        );
    }

    #[test]
    fn audit_method_call_drops_tracking() {
        // A method call on the bypass-constructed instance may
        // initialise fields internally — drop tracking
        // conservatively to avoid false positives.
        let src = "\
class ApiClient:
    api_key: str
    base_url: str

impl ApiClient:
    def configure(self) -> None:
        pass

def make() -> ApiClient:
    let c: ApiClient = ApiClient.__new__(ApiClient)
    c.configure()
    return c
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "method call must drop tracking: {:?}",
            d.errors()
        );
    }

    #[test]
    fn audit_rebinding_drops_tracking() {
        let src = "\
class ApiClient:
    api_key: str
    base_url: str

def make() -> ApiClient:
    let c: ApiClient = ApiClient.__new__(ApiClient)
    let c: ApiClient = ApiClient(api_key=\"x\", base_url=\"y\")
    return c
";
        let d = check(src);
        // Note: rebinding `let c` twice would normally trip `let`
        // immutability; this test exists to verify the audit no-ops
        // when the original tracked binding is reassigned to a
        // proper constructor result. The let-reassignment error is
        // a separate concern not in this audit's scope.
        let only_missing_field_init = d
            .errors()
            .iter()
            .all(|e| !matches!(e, TycError::MissingFieldInit { .. }));
        assert!(
            only_missing_field_init,
            "rebinding must clear tracking: {:?}",
            d.errors()
        );
    }

    #[test]
    fn audit_normal_constructor_unaffected() {
        let src = "\
class ApiClient:
    api_key: str
    base_url: str

def make() -> ApiClient:
    let c: ApiClient = ApiClient(api_key=\"x\", base_url=\"y\")
    return c
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    // ── PR-review-driven audit regressions ────────────────────────

    #[test]
    fn audit_return_attribute_access_no_false_positive() {
        // `return c.base_url` reads off the instance, doesn't escape
        // it. Previously the audit collected `c` from the attribute
        // receiver and fired even though only the field flows out.
        // FINDINGS — gemini medium review of v0.2.0.
        let src = "\
class ApiClient:
    api_key: str
    base_url: str

def get_url() -> str:
    let c: ApiClient = ApiClient.__new__(ApiClient)
    c.base_url = \"x\"
    return c.base_url
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "return c.field must not flag c as escaping: {:?}",
            d.errors()
        );
    }

    #[test]
    fn audit_call_arg_attribute_access_no_false_positive() {
        let src = "\
class ApiClient:
    api_key: str
    base_url: str

def consume(s: str) -> None:
    pass

def f() -> None:
    let c: ApiClient = ApiClient.__new__(ApiClient)
    c.base_url = \"x\"
    consume(c.base_url)
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "f(c.field) must not flag c as escaping: {:?}",
            d.errors()
        );
    }

    #[test]
    fn audit_setattr_kwarg_form_drops_tracking() {
        // `setattr(obj=c, name=..., value=...)` — the binding `c` is
        // bound by name, not position. FINDINGS — gemini medium
        // review of v0.2.0.
        let src = "\
class ApiClient:
    api_key: str
    base_url: str

def make() -> ApiClient:
    let c: ApiClient = ApiClient.__new__(ApiClient)
    setattr(obj=c, name=\"api_key\", value=\"x\")
    setattr(obj=c, name=\"base_url\", value=\"y\")
    return c
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "setattr(obj=...) must drop audit tracking: {:?}",
            d.errors()
        );
    }

    #[test]
    fn audit_no_duplicate_diagnostic_for_repeated_name() {
        // `return c if cond else c` — the binding name appears twice
        // in the escape expression. Should emit ONE diagnostic, not
        // two. FINDINGS — copilot review of v0.2.0.
        let src = "\
class ApiClient:
    api_key: str
    base_url: str

def f(cond: bool) -> ApiClient:
    let c: ApiClient = ApiClient.__new__(ApiClient)
    return c if cond else c
";
        let d = check(src);
        let missing_count = d
            .errors()
            .iter()
            .filter(|e| matches!(e, TycError::MissingFieldInit { .. }))
            .count();
        assert_eq!(
            missing_count, 1,
            "exactly one diagnostic expected; got {missing_count}"
        );
    }

    // ── FINDINGS #72: bare collection annotations are implicit-any ────

    #[test]
    fn bare_list_annotation_errors() {
        let src = "def main() -> None:\n    let xs: list = [1, 2, 3]\n";
        let d = check(src);
        assert!(
            d.errors()
                .iter()
                .any(|e| matches!(e, TycError::ImplicitAny { kind, .. } if kind == "list")),
            "expected ImplicitAny for `list`; got {:?}",
            d.errors()
        );
    }

    #[test]
    fn bare_dict_annotation_errors() {
        let src = "def main() -> None:\n    let d: dict = {\"a\": 1}\n";
        let d = check(src);
        assert!(
            d.errors()
                .iter()
                .any(|e| matches!(e, TycError::ImplicitAny { kind, .. } if kind == "dict")),
            "expected ImplicitAny for `dict`; got {:?}",
            d.errors()
        );
    }

    #[test]
    fn bare_tuple_annotation_errors() {
        let src = "def main() -> None:\n    let t: tuple = (1, 2)\n";
        let d = check(src);
        assert!(
            d.errors()
                .iter()
                .any(|e| matches!(e, TycError::ImplicitAny { kind, .. } if kind == "tuple")),
            "expected ImplicitAny for `tuple`; got {:?}",
            d.errors()
        );
    }

    #[test]
    fn parameterised_collection_annotation_is_clean() {
        // Subscripted forms must NOT fire — they carry explicit element types.
        let src = "def main() -> None:\n    let xs: list[int] = [1, 2, 3]\n";
        let d = check(src);
        assert!(
            !d.errors()
                .iter()
                .any(|e| matches!(e, TycError::ImplicitAny { .. })),
            "list[int] must not fire implicit_any: {:?}",
            d.errors()
        );
    }

    // ── FINDINGS #68: generic ctor inference from sealed-union target ──

    #[test]
    fn generic_ctor_pins_tvar_from_sealed_union_target() {
        let src = "\
class Just[T]:
    value: T
class Nothing:
    pass

type Maybe[T] = Just[T] | Nothing

def unwrap(m: Maybe[int]) -> int:
    return 0

let r: int = unwrap(Just(value=5))
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "Just(value=5) under Maybe[int] should pin T=int: {:?}",
            d.errors()
        );
    }

    // ── FINDINGS #69: None as TypeVar value ───────────────────────────

    #[test]
    fn none_arg_binds_to_typevar() {
        // FINDINGS #69: `def f[T](x: T) -> T` should accept `None` as
        // the argument and bind T = None. Pre-fix the call-site
        // nullable-into-non-nullable check fired because `T` reports
        // itself as non-nullable.
        let src = "\
def f[T](x: T) -> T:
    return x
let r: None = f(None)
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "None should bind to TypeVar T: {:?}",
            d.errors()
        );
    }

    // ── FINDINGS #86: *args type check vs kw-only ─────────────────────

    #[test]
    fn varargs_absorb_extra_positionals_against_correct_type() {
        // Repro: `*args: int` should absorb the trailing `2, 3, 4`
        // positional args; `sep="-"` matches the kw-only parameter.
        // Pre-fix, the loop checked `2` against the next listed
        // parameter (`sep: str`) and emitted a spurious type_mismatch.
        let src = "\
def stars(n: int, *args: int, sep: str = \",\", **kwargs: int) -> str:
    return sep
let r: str = stars(1, 2, 3, 4, sep=\"-\")
";
        let d = check(src);
        assert!(
            !d.errors()
                .iter()
                .any(|e| matches!(e, TycError::TypeMismatch { .. })),
            "*args absorbing positionals must not fire type_mismatch: {:?}",
            d.errors()
        );
    }

    #[test]
    fn varargs_check_element_type_against_extras() {
        // When *args is `int`, passing a `str` should still error
        // (but with the right expected type — the vararg's element type).
        let src = "\
def stars(n: int, *args: int) -> int:
    return n
let r: int = stars(1, \"bad\")
";
        let d = check(src);
        assert!(
            d.errors()
                .iter()
                .any(|e| matches!(e, TycError::TypeMismatch { expected, .. } if expected == "int")),
            "vararg type check should fire when extra arg doesn't match: {:?}",
            d.errors()
        );
    }

    // ── FINDINGS #82: missing-return analysis ─────────────────────────

    #[test]
    fn missing_return_on_some_paths_errors() {
        let src = "\
def maybe_int(x: int) -> int:
    if x > 0:
        return x
";
        let d = check(src);
        assert!(d.has_errors(), "expected missing-return diagnostic");
        assert!(
            d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingReturn { .. })),
            "expected MissingReturn variant, got {:?}",
            d.errors()
        );
    }

    #[test]
    fn return_on_every_path_is_clean() {
        let src = "\
def f(x: int) -> int:
    if x > 0:
        return x
    return 0
";
        let d = check(src);
        assert!(
            !d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingReturn { .. })),
            "every-path return must not fire missing_return: {:?}",
            d.errors()
        );
    }

    #[test]
    fn void_function_without_return_is_clean() {
        let src = "\
def f() -> None:
    print(1)
";
        let d = check(src);
        assert!(
            !d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingReturn { .. })),
            "void function must not fire missing_return: {:?}",
            d.errors()
        );
    }

    #[test]
    fn nullable_return_without_explicit_none_is_clean() {
        // Declared `int | None`: falling off the end yields `None`,
        // which is a legal value for the declared type.
        let src = "\
def f(x: int) -> int | None:
    if x > 0:
        return x
";
        let d = check(src);
        assert!(
            !d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingReturn { .. })),
            "T | None must not fire missing_return: {:?}",
            d.errors()
        );
    }

    #[test]
    fn try_except_both_return_is_clean() {
        // `try: return Ok(...) except E: return Err(...)` — both the try body
        // and the exception handler return unconditionally, so the function
        // always exits and must NOT fire missing_return.
        let src = "\
def parse(raw: str) -> Result[int, str]:
    try:
        return Ok(int(raw))
    except ValueError as e:
        return Err(str(e))
";
        let d = check(src);
        assert!(
            !d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingReturn { .. })),
            "try/except with returns in both branches must not fire missing_return: {:?}",
            d.errors()
        );
    }

    #[test]
    fn try_without_handler_return_fires_missing_return() {
        // The try body returns but the handler does NOT — missing_return must fire.
        let src = "\
def parse(raw: str) -> int:
    try:
        return int(raw)
    except ValueError as e:
        print(str(e))
";
        let d = check(src);
        assert!(
            d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingReturn { .. })),
            "try with non-exiting handler must fire missing_return: {:?}",
            d.errors()
        );
    }

    #[test]
    fn try_finally_always_exits_is_clean() {
        // A `finally` block that always returns exits on every path.
        let src = "\
def load() -> int:
    try:
        return 1
    except Exception:
        print(\"err\")
    finally:
        return 0
";
        let d = check(src);
        assert!(
            !d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingReturn { .. })),
            "try/finally with returning finally must not fire missing_return: {:?}",
            d.errors()
        );
    }

    #[test]
    fn interface_stub_body_is_clean() {
        // Protocol / interface method declarations end in `...` —
        // missing_return must not fire on a stub body.
        let src = "\
interface Drawable:
    def area(self) -> float:
        ...
";
        let d = check(src);
        assert!(
            !d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingReturn { .. })),
            "interface stub must not fire missing_return: {:?}",
            d.errors()
        );
    }

    #[test]
    fn raise_on_fallthrough_is_clean() {
        // A function that always raises on every path satisfies the
        // missing-return analysis even without a `return`.
        let src = "\
def fail() -> int:
    raise ValueError(\"x\")
";
        let d = check(src);
        assert!(
            !d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingReturn { .. })),
            "raise must satisfy missing_return: {:?}",
            d.errors()
        );
    }

    #[test]
    fn non_exhaustive_match_without_catchall_fires_missing_return() {
        // Copilot review on PR #68 (tyc-types L3558): a `match` over an
        // open-typed subject (`int` here) without a `case _:` arm can
        // fall through at runtime — missing-return must fire even when
        // every present arm exits.
        let src = "\
def f(n: int) -> int:
    match n:
        case 0:
            return 1
";
        let d = check(src);
        assert!(
            d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingReturn { .. })),
            "non-exhaustive match must fire missing_return; got {:?}",
            d.errors()
        );
    }

    #[test]
    fn match_with_catchall_satisfies_missing_return() {
        let src = "\
def f(n: int) -> int:
    match n:
        case 0:
            return 1
        case _:
            return -1
";
        let d = check(src);
        assert!(
            !d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingReturn { .. })),
            "catch-all should satisfy missing_return: {:?}",
            d.errors()
        );
    }

    #[test]
    fn exhaustive_sealed_match_without_wildcard_satisfies_missing_return() {
        // Sealed-union exhaustiveness over Shape (Circle | Square) is
        // proven by listing every variant. Missing_return must respect
        // that and not fire even without a trailing `case _:`.
        let src = "\
class Circle:
    r: float
class Square:
    s: float

type Shape = Circle | Square

def area(s: Shape) -> float:
    match s:
        case Circle():
            return 1.0
        case Square():
            return 2.0
";
        let d = check(src);
        assert!(
            !d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingReturn { .. })),
            "exhaustive sealed-union match should satisfy missing_return: {:?}",
            d.errors()
        );
    }

    #[test]
    fn unsafe_block_with_inner_returns_satisfies_missing_return() {
        // F2: `unsafe:` lowers to `if True:`, which is unconditional
        // flow — when every path inside the block returns/raises, the
        // function returns. Missing_return must not fire.
        let src = "\
def f(x: object) -> int:
    unsafe:
        if isinstance(x, int):
            return x
        raise ValueError(\"bad\")
";
        let d = check(src);
        assert!(
            !d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingReturn { .. })),
            "unsafe block with terminal raise must satisfy missing_return: {:?}",
            d.errors()
        );
    }

    #[test]
    fn with_block_returning_body_satisfies_missing_return() {
        // F3: a `with` block whose body's last statement is a return
        // is a definite return of the enclosing function.
        let src = "\
def f() -> int:
    with open(\"x\") as g:
        return 1
";
        let d = check(src);
        assert!(
            !d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingReturn { .. })),
            "with block ending in return must satisfy missing_return: {:?}",
            d.errors()
        );
    }

    #[test]
    fn with_block_raising_body_still_fires_missing_return() {
        // Codex PR #73 review: a `with` body that ends in `raise`
        // is NOT a definite function exit because the context
        // manager's `__exit__` can suppress the exception
        // (`contextlib.suppress(Exception)`, custom managers
        // returning truthy). Missing-return must still fire.
        let src = "\
from contextlib import suppress

def f() -> int:
    with suppress(Exception):
        raise ValueError(\"x\")
";
        let d = check(src);
        assert!(
            d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingReturn { .. })),
            "with+raise must still fire missing_return: {:?}",
            d.errors()
        );
    }

    #[test]
    fn exhaustive_result_match_satisfies_missing_return() {
        // F4: `match` over `Result[T, E]` covering both `Ok` and `Err`
        // arms (each ending in a return) is a definite return.
        let src = "\
def f(r: Result[int, str]) -> int:
    match r:
        case Ok(v):
            return v
        case Err(_):
            return -1
";
        let d = check(src);
        assert!(
            !d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingReturn { .. })),
            "exhaustive Result match must satisfy missing_return: {:?}",
            d.errors()
        );
    }

    // ── FINDINGS #80: typo'd kwarg surfaces tyc::unknown_kwarg ────────

    #[test]
    fn typo_kwarg_emits_unknown_kwarg_with_suggestion() {
        let src = "\
def greet(name: str, greeting: str = \"Hello\") -> str:
    return greeting + \", \" + name

let r: str = greet(\"Amy\", greetinx=\"Hi\")
";
        let d = check(src);
        assert!(d.has_errors());
        let err = d
            .errors()
            .iter()
            .find(|e| matches!(e, TycError::UnknownKwarg { .. }))
            .expect("expected UnknownKwarg variant");
        let msg = format!("{}", err);
        assert!(
            msg.contains("greetinx") && msg.contains("greet"),
            "headline should name kwarg and function; got: {msg}"
        );
        // The suggestion should pick up the close match `greeting`.
        if let TycError::UnknownKwarg { suggestion, .. } = err {
            assert!(
                suggestion.contains("greeting"),
                "suggestion should propose `greeting`; got: {suggestion}"
            );
        }
    }

    #[test]
    fn unknown_kwarg_lists_candidates_when_no_close_match() {
        // Random kwarg that's nowhere near any parameter should fall
        // back to listing every accepted parameter name.
        let src = "\
def greet(name: str, greeting: str = \"Hello\") -> str:
    return greeting + \", \" + name

let r: str = greet(\"Amy\", xyzzy=\"Hi\")
";
        let d = check(src);
        let err = d
            .errors()
            .iter()
            .find(|e| matches!(e, TycError::UnknownKwarg { .. }))
            .expect("expected UnknownKwarg variant");
        if let TycError::UnknownKwarg { suggestion, .. } = err {
            assert!(
                suggestion.contains("name") && suggestion.contains("greeting"),
                "suggestion should list accepted params; got: {suggestion}"
            );
        }
    }

    #[test]
    fn function_with_double_star_kwarg_accepts_arbitrary_names() {
        // Functions declared with `**kwargs` should never emit
        // unknown_kwarg — anything is legal.
        let src = "\
def greet(name: str, **extras: str) -> str:
    return name

let r: str = greet(\"Amy\", weird_name=\"Hi\", another=\"x\")
";
        let d = check(src);
        assert!(
            !d.errors()
                .iter()
                .any(|e| matches!(e, TycError::UnknownKwarg { .. })),
            "**kwargs must absorb arbitrary names; got {:?}",
            d.errors()
        );
    }

    // ── newtype (Phase A) ───────────────────────────────────────────────

    #[test]
    fn newtype_accepts_explicit_construction() {
        let src = "\
newtype UserId = int
def greet(uid: UserId) -> str:
    return \"hi\"
let me: UserId = UserId(7)
let _msg: str = greet(me)
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn newtype_rejects_bare_base_value() {
        let src = "\
newtype UserId = int
def greet(uid: UserId) -> str:
    return \"hi\"
let raw: int = 7
let _msg: str = greet(raw)
";
        let d = check(src);
        assert!(d.has_errors(), "expected type mismatch on bare int → UserId");
    }

    #[test]
    fn newtype_allows_escape_upward_to_base() {
        let src = "\
newtype UserId = int
def double(n: int) -> int:
    return n * 2
let me: UserId = UserId(7)
let _twice: int = double(me)
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn newtype_rejects_cross_newtype_assignment() {
        let src = "\
newtype UserId = int
newtype PostId = int
def greet(uid: UserId) -> str:
    return \"hi\"
let post: PostId = PostId(42)
let _msg: str = greet(post)
";
        let d = check(src);
        assert!(d.has_errors(), "expected type mismatch on PostId → UserId");
    }

    #[test]
    fn newtype_constructor_arg_type_checked() {
        let src = "\
newtype UserId = int
let bad: UserId = UserId(\"seven\")
";
        let d = check(src);
        assert!(d.has_errors(), "expected newtype_violation for str arg");
    }

    // ── blocking in async (Phase E) ─────────────────────────────────────

    #[test]
    fn blocking_call_in_async_def_warns() {
        let src = "\
import time
async def bad() -> None:
    time.sleep(1)
    return
";
        let d = check(src);
        assert!(
            d.warnings()
                .iter()
                .any(|w| matches!(w, TycError::BlockingInAsync { .. })),
            "expected blocking_in_async for time.sleep"
        );
    }

    #[test]
    fn requests_get_in_async_def_warns() {
        let src = "\
import requests
async def fetch() -> str:
    let r = requests.get(\"http://x\")
    return \"ok\"
";
        let d = check(src);
        assert!(
            d.warnings()
                .iter()
                .any(|w| matches!(w, TycError::BlockingInAsync { .. })),
            "expected blocking_in_async for requests.get"
        );
    }

    #[test]
    fn asyncio_to_thread_wrapper_does_not_warn() {
        // `asyncio.to_thread(time.sleep, 1)` itself is the wrapper;
        // the dotted-callee match only sees `asyncio.to_thread`,
        // which isn't in BLOCKING_CALLEES, so no warning fires.
        let src = "\
import asyncio
import time
async def good() -> None:
    await asyncio.to_thread(time.sleep, 1)
";
        let d = check(src);
        assert!(
            !d.warnings()
                .iter()
                .any(|w| matches!(w, TycError::BlockingInAsync { .. })),
            "asyncio.to_thread wrapper should not trip blocking_in_async"
        );
    }

    #[test]
    fn blocking_call_in_sync_def_does_not_warn() {
        // The blocking-in-async check only fires inside `async def`;
        // sync functions can call `time.sleep` freely.
        let src = "\
import time
def sync_caller() -> None:
    time.sleep(1)
";
        let d = check(src);
        assert!(
            !d.warnings()
                .iter()
                .any(|w| matches!(w, TycError::BlockingInAsync { .. })),
            "sync function should not trip blocking_in_async"
        );
    }

    #[test]
    fn unsafe_block_suppresses_blocking_warning() {
        let src = "\
import time
async def escape_hatch() -> None:
    unsafe:
        time.sleep(1)
    return
";
        let d = check(src);
        assert!(
            !d.warnings()
                .iter()
                .any(|w| matches!(w, TycError::BlockingInAsync { .. })),
            "unsafe: should suppress blocking_in_async"
        );
    }

    // ── resource discipline (Phase C) ───────────────────────────────────

    #[test]
    fn bare_open_assignment_warns() {
        let src = "\
def read_file(path: str) -> str:
    let f = open(path)
    return f.read()
";
        let d = check(src);
        assert!(
            d.warnings()
                .iter()
                .any(|w| matches!(w, TycError::ResourceNotManaged { .. })),
            "expected resource_not_managed warning"
        );
    }

    #[test]
    fn with_open_does_not_warn() {
        let src = "\
def read_file(path: str) -> str:
    with open(path) as f:
        return f.read()
";
        let d = check(src);
        assert!(
            !d.warnings()
                .iter()
                .any(|w| matches!(w, TycError::ResourceNotManaged { .. })),
            "with-statement should not trip resource discipline"
        );
    }

    #[test]
    fn socket_socket_assignment_warns() {
        let src = "\
import socket
def listen() -> None:
    let s = socket.socket()
    s.close()
";
        let d = check(src);
        assert!(
            d.warnings()
                .iter()
                .any(|w| matches!(w, TycError::ResourceNotManaged { .. })),
            "expected resource_not_managed for socket.socket"
        );
    }

    #[test]
    fn unsafe_block_suppresses_resource_warning() {
        let src = "\
def escape_hatch(path: str) -> None:
    unsafe:
        let f = open(path)
        f.close()
";
        let d = check(src);
        assert!(
            !d.warnings()
                .iter()
                .any(|w| matches!(w, TycError::ResourceNotManaged { .. })),
            "unsafe: should suppress resource discipline"
        );
    }

    #[test]
    fn annotated_resource_assignment_warns() {
        let src = "\
def read_file(path: str) -> str:
    let f: object = open(path)
    return \"\"
";
        let d = check(src);
        assert!(
            d.warnings()
                .iter()
                .any(|w| matches!(w, TycError::ResourceNotManaged { .. })),
            "expected resource_not_managed for annotated assign"
        );
    }

    // ── div-by-zero literal (Phase B) ───────────────────────────────────

    #[test]
    fn div_by_literal_zero_errors() {
        let src = "let r: float = 1 / 0\n";
        let d = check(src);
        assert!(d.has_errors(), "expected div_by_zero_literal");
    }

    #[test]
    fn floor_div_by_literal_zero_errors() {
        let src = "let r: int = 7 // 0\n";
        let d = check(src);
        assert!(d.has_errors(), "expected div_by_zero_literal for //");
    }

    #[test]
    fn mod_by_literal_zero_errors() {
        let src = "let r: int = 7 % 0\n";
        let d = check(src);
        assert!(d.has_errors(), "expected div_by_zero_literal for %");
    }

    #[test]
    fn div_by_negative_zero_literal_errors() {
        let src = "let r: float = 1 / -0\n";
        let d = check(src);
        assert!(d.has_errors(), "expected div_by_zero_literal for -0");
    }

    #[test]
    fn div_by_float_zero_literal_errors() {
        let src = "let r: float = 1.0 / 0.0\n";
        let d = check(src);
        assert!(d.has_errors(), "expected div_by_zero_literal for 0.0");
    }

    #[test]
    fn div_by_nonzero_literal_ok() {
        let src = "let r: float = 1 / 2\n";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn div_by_runtime_value_ok() {
        // Flow-sensitive analysis is out of scope: a runtime value
        // that could be zero is *not* flagged. Keeps the check
        // false-positive-free.
        let src = "\
def f(d: int) -> float:
    return 1 / d
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
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

let p: Point = Point(x=1, y=2)
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

let s: Shape = Circle(radius=1)
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

let s: Shape = Circle(radius=1)

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

let s: Shape = Circle(radius=1)

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

let s: Shape = Circle(radius=1)

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

let s: Shape = Circle(radius=1)

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

let s: Shape = Circle(radius=1)

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

let s: Shape = Circle(radius=1)

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

let x: Shape? = Circle(radius=1)
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

let c = Circle(radius=1.0)
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
    fn variadic_tuple_accepts_fixed_length_literals() {
        // `tuple[T, ...]` is the homogeneous-variadic tuple type — same
        // element type at every position, length unconstrained. The
        // unifier must accept any fixed-length tuple literal whose
        // elements are all assignable to T (FINDINGS O3).
        let d = check("let xs: tuple[float, ...] = (1.0, 2.0, 3.0)\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
        let d = check("let ys: tuple[float, ...] = ()\n");
        assert!(!d.has_errors(), "empty tuple should fit: {:?}", d.errors());
        let d = check("let zs: tuple[int, ...] = (1, 2, 3, 4, 5)\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn variadic_tuple_widens_int_literals_to_float() {
        // The element-type hint flows into the tuple literal's slots
        // exactly like a fixed-arity expectation would, so int literals
        // widen to float when the target is `tuple[float, ...]`.
        let d = check("let xs: tuple[float, ...] = (1, 2, 3)\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn variadic_tuple_rejects_mismatched_element() {
        // A wrong element type must still surface — the variadic
        // marker only relaxes arity, not the per-element type check.
        let d = check("let xs: tuple[float, ...] = (1.0, \"oops\", 3.0)\n");
        assert!(d.has_errors(), "expected str-vs-float to be rejected");
        // The diagnostic should render the expected type in its source
        // form (`tuple[T, ...]`), not the internal head name.
        let msg = format!("{}", d.errors()[0]);
        assert!(
            msg.contains("tuple[float, ...]"),
            "diagnostic should render variadic tuple as `tuple[float, ...]`; got: {}",
            msg,
        );
    }

    #[test]
    fn generator_return_no_value_accepted_against_iterator() {
        // Inside a generator, `return` is `raise StopIteration` and
        // produces no `Iterator[T]` value — the return-statement
        // validator must skip its usual assignability check
        // (FINDINGS O6).
        let src = "\
from typing import Iterator

def stop_early(n: int) -> Iterator[int]:
    for i in range(n):
        if i > 5:
            return
        yield i
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "bare `return` inside an Iterator[T] generator must be accepted; got: {:?}",
            d.errors()
        );
    }

    #[test]
    fn while_loop_test_narrows_body_for_iterator_idiom() {
        // The linked-list iterator pattern relies on `while cur is not
        // None:` narrowing `cur` to non-null at every read site inside
        // the body — including the `cur = cur.next` reassignment
        // (which reads .next on the *currently narrowed* value, then
        // resets narrowing for the new value). Without while-test
        // narrowing this is `tyc::nullable_use` on the very first
        // `cur.value` and `cur.next` read (FINDINGS O2).
        let src = "\
class Node:
    value: int
    next: Node?

def sum_list(head: Node?) -> int:
    mut total: int = 0
    mut cur: Node? = head
    while cur is not None:
        total = total + cur.value
        cur = cur.next
    return total
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "while-test narrowing must let the linked-list idiom check; got: {:?}",
            d.errors()
        );
    }

    #[test]
    fn while_loop_narrowing_resets_after_reassignment() {
        // Narrowing inside the body is sound only *until* the narrowed
        // name is reassigned. After `cur = None` the next read must
        // see `cur` as nullable again — the assignment site resets
        // narrowing exactly the way a plain `if`-narrowed branch does.
        let src = "\
class Node:
    value: int
    next: Node?

def f(head: Node?) -> int:
    mut total: int = 0
    mut cur: Node? = head
    while cur is not None:
        cur = None
        total = total + cur.value
    return total
";
        let d = check(src);
        assert!(
            d.has_errors(),
            "post-reassignment read should still trip nullable_use"
        );
    }

    #[test]
    fn recursive_type_alias_emits_one_cycle_error_no_cascade() {
        // Self-referential aliases through `list[Self]` / `dict[str, Self]`
        // are the canonical recursive-JSON / AST / tree shape and aren't
        // yet supported (FINDINGS O4). The diagnostic must fire once
        // for the cycle itself, and every subsequent use of the alias
        // must *not* cascade into a flood of `tyc::type_mismatch`
        // errors — the alias body is rewritten to `Any` so downstream
        // assignments fall through silently.
        let src = "\
type JSON = None | bool | int | float | str | list[JSON] | dict[str, JSON]

def main() -> None:
    let x: JSON = {\"a\": [1, 2, \"three\"]}
    let y: JSON = None
    let z: JSON = [1, 2, 3]
    print(x, y, z)
";
        let d = check(src);
        let errs = d.errors();
        // Exactly one error: the cycle. Cascading type_mismatch errors
        // on every alias use are not acceptable — they bury the real
        // problem.
        let cycle_errs = errs
            .iter()
            .filter(|e| format!("{e}").contains("cycle"))
            .count();
        assert_eq!(
            cycle_errs, 1,
            "expected exactly one cyclic_type_alias error; got: {errs:?}",
        );
        let mismatch_errs = errs
            .iter()
            .filter(|e| format!("{e}").contains("type mismatch"))
            .count();
        assert_eq!(
            mismatch_errs, 0,
            "recursive alias must not cascade into type_mismatch errors; got: {errs:?}",
        );
    }

    #[test]
    fn generator_return_with_value_accepted_against_iterator() {
        // PEP 380: `return value` inside a generator sets
        // StopIteration.value. The body is still a generator, so the
        // declared `Iterator[int]` return type is correct — the value
        // is *not* required to match that type.
        let src = "\
from typing import Iterator

def stop_early() -> Iterator[int]:
    yield 1
    return \"done\"
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "`return value` inside a generator must skip the value-against-Iterator check; got: {:?}",
            d.errors()
        );
    }

    #[test]
    fn generator_return_value_checked_against_generator_r_param() {
        // `Generator[Y, S, R]` *does* carry a declared return-payload
        // type; a `return value` inside one must be assignable to R.
        // The Iterator-shaped relaxation must not silently accept
        // mismatched payloads when the user spelled out all three
        // parameters (Codex review on PR #94).
        let ok = check(
            "\
from typing import Generator

def g() -> Generator[int, None, str]:
    yield 1
    return \"done\"
",
        );
        assert!(
            !ok.has_errors(),
            "matching `return value` against R should be accepted; got: {:?}",
            ok.errors()
        );

        let bad = check(
            "\
from typing import Generator

def g() -> Generator[int, None, str]:
    yield 1
    return 42
",
        );
        assert!(
            bad.has_errors(),
            "mismatched `return value` against R should be rejected"
        );
        let msg = format!("{}", bad.errors()[0]);
        assert!(
            msg.contains("expected `str`"),
            "diagnostic should reference R (= str); got: {}",
            msg,
        );
    }

    #[test]
    fn while_else_branch_has_negated_narrowing() {
        // `while x is not None: ... else: <here>` runs exactly when
        // the test became false — i.e. `x is None`. The else block
        // should see `x` narrowed to `None`, the dual of the body's
        // positive narrowing (Gemini review on PR #94).
        let src = "\
def f(x: int?) -> None:
    mut cur: int? = x
    while cur is not None:
        cur = None
    else:
        let n: None = cur
        return
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "negated narrowing should flow into the `while … else:` block; got: {:?}",
            d.errors()
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

let d: Named = Dog(name=\"Fido\")
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

let d: Named = Dog(name=1)
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

let d: Named = Dog(age=1)
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

let c: Config = BadConfig(port=\"oops\")
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

let p: Owner = Person(pet=Dog(name=\"Fido\"))
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

    // ── E2: impl methods accept T? parameters ────────────────────────────────

    #[test]
    fn impl_method_accepts_nullable_arg_and_none() {
        // FINDINGS E2: `impl X: def f(self, p: str?)` rejected `None`
        // and `str?` arguments because the method's `param_types`
        // weren't recorded — they fell back to `Type::Unknown`, and
        // the call site's nullable-into-non-nullable guard misfired.
        let src = "\
class API:
    name: str

impl API:
    def fetch(self, cursor: str?) -> int:
        return 0 if cursor is None else len(cursor)

def main() -> None:
    let api: API = API(name=\"x\")
    let v: str? = None
    let n1: int = api.fetch(v)
    let n2: int = api.fetch(None)
    let n3: int = api.fetch(\"hi\")
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "impl method with `T?` param must accept `T?` / `None`: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    // ── E3: `return self` from impl `__enter__` carries the real class type ──

    #[test]
    fn impl_return_self_uses_real_class_name() {
        // FINDINGS E3: the impl block desugars to `__typhon_impl_X`;
        // the type checker was previously typing `self` as
        // `__typhon_impl_X` so `return self` against `-> X` failed.
        let src = "\
class Stopwatch:
    start: float

impl Stopwatch:
    def enter(self) -> Stopwatch:
        return self
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "return self from impl method must match declared class type: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    // ── E4: type-alias of Callable unwraps on call ──────────────────────────

    #[test]
    fn callable_type_alias_call_returns_alias_target() {
        // FINDINGS E4: `type Handler = Callable[[Req], Resp]` followed
        // by `next(req)` typed the return as `Handler` not `Resp`.
        let src = "\
from typing import Callable

class Req:
    n: int

class Resp:
    s: str

type Handler = Callable[[Req], Resp]

def call(h: Handler, r: Req) -> Resp:
    return h(r)
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "calling a value typed as a Callable alias must return the alias target: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    // ── E5: __mul__ / __add__ overrides resolve to user-declared return ──────

    #[test]
    fn binop_resolves_user_dunder_over_numeric_fallback() {
        // FINDINGS E5: `Vec2(...) * 5.0` resolved to `Float` via the
        // numeric-coercion rule even though `Vec2` defined
        // `__mul__(self, scalar: float) -> Vec2`. The dunder lookup
        // now takes precedence over the conservative numeric
        // inference table.
        let src = "\
class Vec2:
    x: float
    y: float

impl Vec2:
    def __mul__(self, scalar: float) -> Vec2:
        return Vec2(x=self.x * scalar, y=self.y * scalar)
    def __add__(self, other: Vec2) -> Vec2:
        return Vec2(x=self.x + other.x, y=self.y + other.y)

def go() -> None:
    let a: Vec2 = Vec2(x=1.0, y=2.0)
    let b: Vec2 = Vec2(x=3.0, y=4.0)
    let sum: Vec2 = a + b
    let scaled: Vec2 = a * 5.0
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "user-defined __mul__/__add__ must drive BinOp result type: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn binop_dunder_rejects_arg_type_mismatch() {
        // codex review on PR #87: previously, the dunder return type
        // was adopted whenever the dunder method existed, even if the
        // RHS didn't match the dunder's declared formal. `v * "bad"`
        // for `def __mul__(self, scalar: float) -> Vec2` must surface
        // an operator type mismatch, not silently infer `Vec2`.
        let src = "\
class V:
    x: float

impl V:
    def __mul__(self, scalar: float) -> V:
        return V(x=self.x * scalar)

def main() -> None:
    let v: V = V(x=1.0)
    let r: V = v * \"bad\"
";
        let d = check(src);
        assert!(
            d.has_errors(),
            "v * \"bad\" must surface a diagnostic when __mul__ expects a float, got: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    // ── E6: exhaustiveness recognises positional class patterns ──────────────

    #[test]
    fn positional_class_pattern_counts_as_total_match() {
        // FINDINGS E6: `case Leaf(value):` (positional capture of every
        // field) was not treated as covering the `Leaf` variant of a
        // sealed union, so `missing_return` fired on otherwise-total
        // matches.
        let src = "\
class Leaf:
    value: int

class Branch:
    left: Leaf
    right: Leaf

type Tree = Leaf | Branch

def first(t: Tree) -> int:
    match t:
        case Leaf(value):
            return value
        case Branch(left, right):
            return left.value
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "positional class pattern must count as a total match for that variant: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn shorter_positional_pattern_still_covers_variant() {
        // copilot review on PR #87: `case Branch(left):` (one
        // positional capture for a class with two declared fields)
        // is a valid pattern at parse time and should count as a
        // total match for the Branch variant of a sealed union —
        // the omitted positional is unconstrained at runtime.
        let src = "\
class Leaf:
    value: int

class Branch:
    left: Leaf
    right: Leaf

type Tree = Leaf | Branch

def go(t: Tree) -> int:
    match t:
        case Leaf(value):
            return value
        case Branch(left):
            return left.value
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "shorter positional pattern must count as a total match: {:?}",
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

    // ── async_without_await ───────────────────────────────────────────────

    fn has_async_without_await_warning(d: &Diagnostics) -> bool {
        d.warnings().iter().any(|w: &TycError| {
            let s = w.to_string();
            s.contains("async_without_await") || s.contains("no `await`")
        })
    }

    #[test]
    fn async_without_await_warns_on_bare_async_def() {
        let d = check("async def foo():\n    x: int = 1\n");
        assert!(
            has_async_without_await_warning(&d),
            "expected async_without_await warning, got: {:?}",
            d.warnings()
                .iter()
                .map(|w: &TycError| w.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn async_without_await_silent_when_await_present() {
        let d = check("async def fetch() -> int:\n    x = await some_coro()\n    return x\n");
        assert!(
            !has_async_without_await_warning(&d),
            "unexpected async_without_await warning: {:?}",
            d.warnings()
                .iter()
                .map(|w: &TycError| w.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn async_without_await_not_triggered_by_await_in_nested_def() {
        // An `await` that lives inside an inner `async def` does not count
        // toward the outer function's await detection.
        let d = check("async def outer():\n    async def inner():\n        await some_coro()\n");
        assert!(
            has_async_without_await_warning(&d),
            "outer should warn because its own body has no await; got: {:?}",
            d.warnings()
                .iter()
                .map(|w: &TycError| w.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn async_without_await_silent_for_async_for() {
        // `async for` counts as an asynchronous construct — no warning expected.
        let d = check("async def consume(ait):\n    async for item in ait:\n        pass\n");
        assert!(
            !has_async_without_await_warning(&d),
            "async for should suppress async_without_await; got: {:?}",
            d.warnings()
                .iter()
                .map(|w: &TycError| w.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn async_without_await_silent_for_async_with() {
        // `async with` counts as an asynchronous construct — no warning expected.
        let d = check("async def managed(cm):\n    async with cm as ctx:\n        pass\n");
        assert!(
            !has_async_without_await_warning(&d),
            "async with should suppress async_without_await; got: {:?}",
            d.warnings()
                .iter()
                .map(|w: &TycError| w.to_string())
                .collect::<Vec<_>>()
        );
    }

    // ── Phase 5.2: `or` / `and` typing ────────────────────────────────────

    #[test]
    fn or_returns_truthy_lhs_unioned_with_rhs() {
        // The motivating case: a Telegram-style `update.text or ""` pattern
        // where `text: str | None` should produce `str` after the `or`, not
        // `bool`. Without the truthy-LHS rule this binds `bool` to a `str`
        // annotation and errors.
        let src = "\
class Update:
    text: str | None

def handle(update: Update) -> None:
    let s: str = update.text or \"\"
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "`str | None or str` should infer as `str`; got: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn or_keeps_bool_when_both_sides_bool() {
        // The legacy behaviour (BoolOp -> Bool) must still hold for the
        // common `flag or default` shape where both operands are booleans.
        let src = "\
def f(a: bool, b: bool) -> None:
    let c: bool = a or b
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "`bool or bool` should still be `bool`; got: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn and_widens_to_union() {
        // `a and b` evaluates to `a` when falsy, else `b`. The conservative
        // union widening means a same-type `and` keeps the type — assigning
        // `int and int` to an `int` annotation must NOT error.
        let src = "\
def f(a: int, b: int) -> None:
    let c: int = a and b
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "`int and int` should be `int`; got: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn or_chain_regression_maybe_str_or_default() {
        // End-to-end pipeline regression: the canonical Phase 5.2 motivating
        // example. Reads as a free-standing module so the resolver, type
        // narrower, and BoolOp inference all participate.
        let src = "\
let maybe_str: str | None = None
let s: str = maybe_str or \"default\"
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "Phase 5.2 regression: `(str | None) or str` should be `str`; got: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    // ── Phase 5.2: Generator → Iterable conformance ───────────────────────

    #[test]
    fn generator_assignable_to_iterable_of_same_element() {
        // `Generator[int, None, None]` should satisfy `Iterable[int]` — the
        // shape `yield`-bodies most often have.
        let g = Type::Generic("Generator".into(), vec![Type::Int, Type::None, Type::None]);
        let it = Type::Generic("Iterable".into(), vec![Type::Int]);
        assert!(
            assignable(&it, &g),
            "Generator[int, ...] should be assignable to Iterable[int]"
        );
    }

    #[test]
    fn generator_assignable_to_iterator() {
        // `Iterator[T]` is the other common return annotation.
        let g = Type::Generic("Generator".into(), vec![Type::Str, Type::None, Type::None]);
        let it = Type::Generic("Iterator".into(), vec![Type::Str]);
        assert!(
            assignable(&it, &g),
            "Generator[str, ...] should be assignable to Iterator[str]"
        );
    }

    #[test]
    fn async_generator_assignable_to_async_iterable() {
        let ag = Type::Generic("AsyncGenerator".into(), vec![Type::Int, Type::None]);
        let ait = Type::Generic("AsyncIterable".into(), vec![Type::Int]);
        let aiter = Type::Generic("AsyncIterator".into(), vec![Type::Int]);
        assert!(
            assignable(&ait, &ag),
            "AsyncGenerator[int, ...] should be assignable to AsyncIterable[int]"
        );
        assert!(
            assignable(&aiter, &ag),
            "AsyncGenerator[int, ...] should be assignable to AsyncIterator[int]"
        );
    }

    #[test]
    fn generator_function_returning_iterable_type_checks() {
        // Full pipeline: a `yield`-bodied function annotated as
        // `Iterable[int]` must not error. Without the conformance rule the
        // body infers as `Generator[int, ...]` and fails the return check.
        let src = "\
def numbers() -> Iterable[int]:
    yield 1
    yield 2
";
        let d = check(src);
        assert!(
            !d.has_errors(),
            "yield-bodied -> Iterable[int] should type-check; got: {:?}",
            d.errors().iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }
}
