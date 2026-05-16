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

use rustpython_ast::text_size::TextRange;
use rustpython_ast::{Constant, Expr, MatchCase, Mod, Operator, Pattern, Ranged, Stmt};
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
/// - Generic types match on the head name and check each arg pairwise.
/// - Otherwise structural equality.
pub fn assignable(expected: &Type, actual: &Type) -> bool {
    match (expected, actual) {
        (Type::Any, _) | (_, Type::Any) => true,
        (Type::Unknown, _) | (_, Type::Unknown) => true,
        (Type::Float, Type::Int) => true,
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
            an == bn && aa.len() == bb.len() && aa.iter().zip(bb).all(|(x, y)| assignable(x, y))
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
        (a, b) => a == b,
    }
}

/// Translate an annotation expression into a [`Type`].
///
/// `classes` is the set of class names declared in the enclosing module so
/// we can resolve nominal references.
pub fn type_from_annotation(expr: &Expr<TextRange>, classes: &[String]) -> Type {
    type_from_annotation_with_params(expr, classes, &[])
}

/// Same as [`type_from_annotation`] but treats every name in `type_params`
/// as `Type::Any` so that PEP 695 generic functions don't trip the
/// assignability check before we have a real inference engine.
pub fn type_from_annotation_with_params(
    expr: &Expr<TextRange>,
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
            // A type parameter (PEP 695) — treat as a top type until we
            // have proper inference.
            other if type_params.iter().any(|p| p == other) => Type::Any,
            other if classes.iter().any(|c| c == other) => Type::Class(other.to_owned()),
            // Unknown but treat as nominal class (may be imported).
            other => Type::Class(other.to_owned()),
        },
        Expr::BinOp(b) if matches!(b.op, rustpython_ast::Operator::BitOr) => {
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
        Expr::Constant(c) if matches!(c.value, Constant::None) => Type::None,
        _ => Type::Unknown,
    }
}

/// Collect the names of PEP 695 type parameters into a flat list.
pub fn collect_type_param_names(
    type_params: &[rustpython_ast::TypeParam<TextRange>],
) -> Vec<String> {
    type_params
        .iter()
        .map(|tp| match tp {
            rustpython_ast::TypeParam::TypeVar(t) => t.name.as_str().to_owned(),
            rustpython_ast::TypeParam::ParamSpec(p) => p.name.as_str().to_owned(),
            rustpython_ast::TypeParam::TypeVarTuple(t) => t.name.as_str().to_owned(),
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
    env: TypeEnv,
    diagnostics: Diagnostics,
    /// Return type of the function whose body we are currently checking
    /// (None at module scope).
    current_return: Option<Type>,
    /// Reserved for the no-implicit-Any region check: each `unsafe:` block
    /// bumps this counter so the checker can later permit `Any` to bind
    /// freely inside while still requiring an explicit annotation at the
    /// boundary. v1 emits the `if True:` wrapper but doesn't yet use this
    /// field — Phase 3+ will wire it.
    #[allow(dead_code)]
    unsafe_depth: u32,
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

/// Member shape recorded for an interface or class — methods are recorded as
/// their parameter count (excluding `self`/`cls`), fields as their declared
/// type.
#[derive(Debug, Clone, Default)]
struct InterfaceShape {
    /// Method name → parameter count (excluding the receiver).
    methods: HashMap<String, usize>,
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
            interfaces: HashMap::new(),
            class_shapes: HashMap::new(),
            unsafe_depth: 0,
            sealed_unions: HashMap::new(),
            env: TypeEnv::default(),
            diagnostics: Diagnostics::new(),
            current_return: None,
        }
    }

    /// Assignment compatibility check that accounts for sealed-union subtyping
    /// and structural conformance against `interface` declarations.
    ///
    /// Extends the module-level [`assignable`] function with three rules:
    ///
    /// 1. **Variant → sealed union**: `Circle` is assignable to `Shape` when
    ///    `type Shape = Circle | Rectangle | ...` is declared.
    /// 2. **Class → interface**: a class is assignable to an `interface` when
    ///    its member shape covers every required member of the interface.
    /// 3. **Union interception**: when `expected` is a `Union`, retry each
    ///    variant with `is_assignable` so the above rules are available inside
    ///    composite types like `Shape | None` or `list[Shape]`.
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
        }
        // For Union expected types (e.g. `Shape | None`), `assignable` recurses
        // using only the base rules. Re-check each variant here so sealed-union
        // knowledge is available in the recursive call.
        if let Type::Union(variants) = expected {
            return variants.iter().any(|v| self.is_assignable(v, actual));
        }
        false
    }

    /// Return `true` if class `cls_name`'s member shape covers every required
    /// member of `iface_name`'s shape (member presence only — full signature
    /// compatibility is a future enhancement).
    fn class_conforms_to_interface(&self, cls_name: &str, iface_name: &str) -> bool {
        let Some(iface) = self.interfaces.get(iface_name) else {
            return false;
        };
        let Some(cls) = self.class_shapes.get(cls_name) else {
            return false;
        };
        for (m, expected_arity) in &iface.shape.methods {
            match cls.methods.get(m) {
                Some(actual_arity) if actual_arity == expected_arity => {}
                _ => return false,
            }
        }
        for f in iface.shape.fields.keys() {
            if !cls.fields.contains_key(f) && !cls.methods.contains_key(f) {
                return false;
            }
        }
        true
    }

    /// Return the missing-member text for a failed interface conformance check.
    /// Returns `None` when the class actually conforms (caller should use
    /// `class_conforms_to_interface` to gate this call).
    fn interface_missing_members(&self, cls_name: &str, iface_name: &str) -> String {
        let iface = match self.interfaces.get(iface_name) {
            Some(i) => i,
            None => return String::new(),
        };
        let cls = self.class_shapes.get(cls_name);
        let mut missing = Vec::new();
        for (m, expected_arity) in &iface.shape.methods {
            match cls.and_then(|c| c.methods.get(m)) {
                Some(actual_arity) if actual_arity == expected_arity => {}
                Some(actual_arity) => missing.push(format!(
                    "{m}(arity {actual_arity}; expected {expected_arity})"
                )),
                None => missing.push(m.clone()),
            }
        }
        for f in iface.shape.fields.keys() {
            let has_field = cls.is_some_and(|c| c.fields.contains_key(f));
            let has_method = cls.is_some_and(|c| c.methods.contains_key(f));
            if !has_field && !has_method {
                missing.push(f.clone());
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

    fn mismatch(&mut self, expected: &Type, actual: &Type, span: (usize, usize)) {
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

    fn interface_isinstance(&mut self, iface: &str, span: (usize, usize)) {
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
}

/// Run the type checker on `module` and return diagnostics.
pub fn check_module(
    path: impl Into<String>,
    source: &str,
    resolved: &ResolvedModule,
    module: &Mod,
) -> Diagnostics {
    let mut c = Checker::new(path.into(), source, resolved);

    if let Mod::Module(m) = module {
        // First pass: collect class names + function signatures so forward
        // references work.
        collect_classes_and_functions(&mut c, &m.body);

        c.env.enter();
        // Seed module scope with collected classes/functions and resolver bindings.
        seed_env_from_scope(&mut c, 0);
        // Seed Typhon built-in names that are not declared in the source:
        // - `env` is a comptime-only function (returns str).
        // - `BaseModel` is injected by the preprocessor for `model` classes.
        // - `Ok`/`Err` may be used before the `from typhon_runtime import`
        //   injection happens (the desugar pass adds it later).
        seed_typhon_builtins(&mut c);
        for stmt in &m.body {
            check_stmt(&mut c, stmt);
        }
        c.env.leave();
    }

    c.diagnostics
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

fn collect_classes_and_functions(c: &mut Checker, body: &[Stmt<TextRange>]) {
    // First pass: collect every class and type-alias *name* into `c.classes`
    // so the subsequent shape and signature passes can resolve nominal
    // references like `field: OtherClass`. Doing the shape collection in
    // the same pass would see an empty class list and treat every nominal
    // type as `Unknown`.
    for stmt in body {
        match stmt {
            Stmt::ClassDef(cd) => {
                c.classes.push(cd.name.as_str().to_owned());
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
    // Third pass: record function signatures (also needs the full class list).
    for stmt in body {
        match stmt {
            Stmt::FunctionDef(f) => {
                let tps = collect_type_param_names(&f.type_params);
                let sig = function_signature(&classes, f.args.as_ref(), f.returns.as_deref(), &tps);
                c.function_signatures
                    .insert(f.name.as_str().to_owned(), sig);
            }
            Stmt::AsyncFunctionDef(f) => {
                let tps = collect_type_param_names(&f.type_params);
                let sig = function_signature(&classes, f.args.as_ref(), f.returns.as_deref(), &tps);
                c.function_signatures
                    .insert(f.name.as_str().to_owned(), sig);
            }
            _ => {}
        }
    }
}

/// `true` if `c` lists `Protocol` in its bases.
fn class_inherits_protocol(c: &rustpython_ast::StmtClassDef<TextRange>) -> bool {
    c.bases
        .iter()
        .any(|b| matches!(b, Expr::Name(n) if n.id.as_str() == "Protocol"))
}

/// `true` if `decorators` includes `@runtime_checkable` (bare or
/// `typing.runtime_checkable`). When set, `isinstance(x, Interface)` is
/// permitted — the protocol opted in to the attribute-presence check.
fn has_runtime_checkable_decorator(decorators: &[Expr<TextRange>]) -> bool {
    decorators.iter().any(|d| {
        let name = match d {
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
fn collect_class_shape(
    cd: &rustpython_ast::StmtClassDef<TextRange>,
    classes: &[String],
) -> InterfaceShape {
    let mut shape = InterfaceShape::default();
    for stmt in &cd.body {
        match stmt {
            Stmt::FunctionDef(f) => {
                let arity = method_arity_excluding_receiver(f.args.as_ref());
                shape.methods.insert(f.name.as_str().to_owned(), arity);
            }
            Stmt::AsyncFunctionDef(f) => {
                let arity = method_arity_excluding_receiver(f.args.as_ref());
                shape.methods.insert(f.name.as_str().to_owned(), arity);
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

fn method_arity_excluding_receiver(args: &rustpython_ast::Arguments<TextRange>) -> usize {
    let total = args.posonlyargs.len() + args.args.len() + args.kwonlyargs.len();
    // Conservatively assume one receiver (`self`/`cls`) when at least one
    // positional argument is present; static methods are uncommon enough that
    // this approximation is acceptable for v1's "member presence" check.
    total.saturating_sub(1)
}

fn function_signature(
    classes: &[String],
    args: &rustpython_ast::Arguments<TextRange>,
    returns: Option<&Expr<TextRange>>,
    type_params: &[String],
) -> Type {
    let mut params = Vec::new();
    let all = args
        .posonlyargs
        .iter()
        .chain(args.args.iter())
        .chain(args.kwonlyargs.iter());
    for arg in all {
        let t = match &arg.def.annotation {
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
        variadic: false,
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

fn check_stmt(c: &mut Checker, stmt: &Stmt<TextRange>) {
    match stmt {
        Stmt::AnnAssign(a) => {
            let ann_type = type_from_annotation(&a.annotation, &c.classes);
            if let Some(value) = &a.value {
                let value_type = infer_expr(c, value);
                if !c.is_assignable(&ann_type, &value_type) {
                    let span = (
                        value.range().start().to_usize(),
                        value.range().end().to_usize(),
                    );
                    c.mismatch(&ann_type, &value_type, span);
                }
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
                if let Expr::Name(n) = target {
                    let span = (
                        n.range.start().to_usize(),
                        n.range.start().to_usize() + n.id.as_str().len(),
                    );
                    let existing = c.env.lookup(n.id.as_str()).cloned();
                    if let Some(b) = existing {
                        // Reassignment: the static type stays as declared;
                        // check the new value fits.
                        if !c.is_assignable(&b.declared, &value_type) {
                            let vspan = (
                                a.value.range().start().to_usize(),
                                a.value.range().end().to_usize(),
                            );
                            c.mismatch(&b.declared, &value_type, vspan);
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
            let tps = collect_type_param_names(&f.type_params);
            check_function(
                c,
                f.name.as_str(),
                &f.args,
                &f.body,
                f.returns.as_deref(),
                &tps,
            )
        }
        Stmt::AsyncFunctionDef(f) => {
            let tps = collect_type_param_names(&f.type_params);
            check_function(
                c,
                f.name.as_str(),
                &f.args,
                &f.body,
                f.returns.as_deref(),
                &tps,
            )
        }
        Stmt::ClassDef(cd) => {
            c.env.enter();
            for s in &cd.body {
                check_stmt(c, s);
            }
            c.env.leave();
        }
        Stmt::Return(ret) => {
            if let (Some(ret_expr), Some(expected)) = (&ret.value, c.current_return.clone()) {
                let value_type = infer_expr(c, ret_expr);
                if !matches!(expected, Type::Unknown) && !c.is_assignable(&expected, &value_type) {
                    let span = (
                        ret_expr.range().start().to_usize(),
                        ret_expr.range().end().to_usize(),
                    );
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
                let rustpython_ast::ExceptHandler::ExceptHandler(h) = h;
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

fn check_function(
    c: &mut Checker,
    name: &str,
    args: &rustpython_ast::Arguments<TextRange>,
    body: &[Stmt<TextRange>],
    returns: Option<&Expr<TextRange>>,
    type_params: &[String],
) {
    let _ = name;
    let classes = c.classes.clone();
    let ret_type = match returns {
        Some(r) => type_from_annotation_with_params(r, &classes, type_params),
        None => Type::Unknown,
    };

    let saved_return = c.current_return.replace(ret_type);
    c.env.enter();

    // Declare parameters with their annotation types. Type parameters resolve
    // to `Any` until a real inference engine lands.
    let all = args
        .posonlyargs
        .iter()
        .chain(args.args.iter())
        .chain(args.kwonlyargs.iter());
    for arg in all {
        let t = match &arg.def.annotation {
            Some(ann) => type_from_annotation_with_params(ann, &classes, type_params),
            None => Type::Unknown,
        };
        let span = (
            arg.def.range.start().to_usize(),
            arg.def.range.start().to_usize() + arg.def.arg.as_str().len(),
        );
        c.env.declare(TypeBinding {
            name: arg.def.arg.as_str().to_owned(),
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
}

fn check_if(c: &mut Checker, i: &rustpython_ast::StmtIf<TextRange>) {
    let _ = infer_expr(c, &i.test);

    // Apply narrowing for the true branch.
    let narrowings = collect_narrowings(c, &i.test, /*negate=*/ false);
    let snap_pre = c.env.snapshot();
    apply_narrowings(c, &narrowings);
    for s in &i.body {
        check_stmt(c, s);
    }
    c.env.restore(snap_pre);

    // Apply opposite narrowing for the else branch.
    let neg = collect_narrowings(c, &i.test, /*negate=*/ true);
    let snap_pre = c.env.snapshot();
    apply_narrowings(c, &neg);
    for s in &i.orelse {
        check_stmt(c, s);
    }
    c.env.restore(snap_pre);
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
fn collect_narrowings(c: &Checker, test: &Expr<TextRange>, negate: bool) -> Vec<Narrowing> {
    let mut out = Vec::new();
    collect_narrowings_inner(c, test, negate, &mut out);
    out
}

fn collect_narrowings_inner(
    c: &Checker,
    test: &Expr<TextRange>,
    negate: bool,
    out: &mut Vec<Narrowing>,
) {
    match test {
        Expr::Compare(cmp) => {
            // x is None / x is not None
            if cmp.ops.len() == 1 && cmp.comparators.len() == 1 {
                let is_op = matches!(cmp.ops[0], rustpython_ast::CmpOp::Is);
                let is_not_op = matches!(cmp.ops[0], rustpython_ast::CmpOp::IsNot);
                if is_op || is_not_op {
                    if let (Expr::Name(n), Expr::Constant(rc)) =
                        (cmp.left.as_ref(), &cmp.comparators[0])
                    {
                        if matches!(rc.value, Constant::None) {
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
        }
        Expr::Call(call) => {
            // isinstance(x, T)
            if let Expr::Name(fn_name) = call.func.as_ref() {
                if fn_name.id.as_str() == "isinstance" && call.args.len() == 2 {
                    if let Expr::Name(target) = &call.args[0] {
                        let new_type = type_from_annotation(&call.args[1], &c.classes);
                        if let Some(b) = c.env.lookup(target.id.as_str()) {
                            let replacement = if negate {
                                // Best-effort: strip the type out of the union.
                                strip_variant(&b.narrowed, &new_type)
                            } else {
                                new_type
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
        Expr::UnaryOp(u) if matches!(u.op, rustpython_ast::UnaryOp::Not) => {
            collect_narrowings_inner(c, &u.operand, !negate, out);
        }
        _ => {}
    }
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
fn infer_expr(c: &mut Checker, expr: &Expr<TextRange>) -> Type {
    match expr {
        Expr::Constant(con) => match &con.value {
            Constant::Bool(_) => Type::Bool,
            Constant::Int(_) => Type::Int,
            Constant::Float(_) => Type::Float,
            Constant::Str(_) => Type::Str,
            Constant::Bytes(_) => Type::Bytes,
            Constant::None => Type::None,
            _ => Type::Unknown,
        },
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
                (Type::Str, Type::Str) if matches!(b.op, rustpython_ast::Operator::Add) => {
                    Type::Str
                }
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
                rustpython_ast::UnaryOp::Not => Type::Bool,
                // Bitwise / arithmetic unary ops preserve the operand type.
                _ => operand,
            }
        }
        Expr::Call(call) => {
            // Ok(value) and Err(error) are Result constructors: infer their
            // types as Generic("Ok", [T]) / Generic("Err", [E]) so that the
            // Result assignability rule in `assignable` can fire.
            if let Expr::Name(fn_name) = call.func.as_ref() {
                let ctor = fn_name.id.as_str();
                if (ctor == "Ok" || ctor == "Err")
                    && call.args.len() == 1
                    && call.keywords.is_empty()
                {
                    let arg_type = infer_expr(c, &call.args[0]);
                    return Type::Generic(ctor.to_owned(), vec![arg_type]);
                }
                // isinstance(x, Interface) is rejected unless the interface
                // explicitly opts in via @runtime_checkable. Runtime Protocol
                // isinstance only checks attribute *presence*, not signature,
                // so we reject the static use by default. Interfaces decorated
                // `@runtime_checkable` are exempt — the user acknowledged the
                // weaker guarantee.
                if fn_name.id.as_str() == "isinstance" && call.args.len() == 2 {
                    if let Expr::Name(t) = &call.args[1] {
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
                    // Argument count check (positional only — conservative).
                    // Variadic functions accept any number of args >= params.len().
                    let count_ok = if variadic {
                        call.args.len() >= params.len()
                    } else {
                        call.args.len() == params.len()
                    };
                    if !count_ok {
                        let name = match call.func.as_ref() {
                            Expr::Name(n) => n.id.as_str().to_owned(),
                            _ => "<call>".to_owned(),
                        };
                        c.wrong_args(&name, params.len(), call.args.len(), call_span);
                    }
                    // Argument type checks (per-pair, ignoring excess).
                    for (i, arg) in call.args.iter().enumerate() {
                        if i >= params.len() {
                            break;
                        }
                        let actual = infer_expr(c, arg);
                        if !c.is_assignable(&params[i], &actual) {
                            let span =
                                (arg.range().start().to_usize(), arg.range().end().to_usize());
                            c.mismatch(&params[i], &actual, span);
                        }
                        // Specifically reject possibly-None args bound to a
                        // non-nullable parameter.
                        if !params[i].is_nullable() && actual.is_nullable() {
                            if let Expr::Name(n) = arg {
                                let span = (
                                    n.range.start().to_usize(),
                                    n.range.start().to_usize() + n.id.as_str().len(),
                                );
                                c.nullable_use(n.id.as_str(), &params[i], span);
                            }
                        }
                    }
                    *ret
                }
                Type::Class(name) => Type::Class(name),
                Type::Unknown | Type::Any => {
                    for a in &call.args {
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
            Type::Unknown
        }
        Expr::Subscript(s) => {
            let _ = infer_expr(c, &s.value);
            let _ = infer_expr(c, &s.slice);
            Type::Unknown
        }
        Expr::List(_) => Type::Generic("list".into(), vec![Type::Unknown]),
        Expr::Tuple(t) => {
            let elts: Vec<Type> = t.elts.iter().map(|e| infer_expr(c, e)).collect();
            Type::Generic("tuple".into(), elts)
        }
        Expr::Dict(_) => Type::Generic("dict".into(), vec![Type::Unknown, Type::Unknown]),
        Expr::Set(_) => Type::Generic("set".into(), vec![Type::Unknown]),
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
fn extract_sealed_union_variants(expr: &Expr<TextRange>) -> Option<Vec<String>> {
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
    cases: &[MatchCase<TextRange>],
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
fn is_wildcard_pattern(pattern: &Pattern<TextRange>) -> bool {
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
fn collect_matched_class_names(pattern: &Pattern<TextRange>, covered: &mut HashSet<String>) {
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
fn bind_pattern_names(c: &mut Checker, pattern: &Pattern<TextRange>) {
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
            for p in &mc.patterns {
                bind_pattern_names(c, p);
            }
            for p in &mc.kwd_patterns {
                bind_pattern_names(c, p);
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
    use rustpython_parser::{parse, Mode};
    use tyc_resolve::resolve_module;
    use tyc_syntax::preprocess::preprocess;

    fn check(src: &str) -> Diagnostics {
        let prep = preprocess(src);
        let module = parse(&prep.python_source, Mode::Module, "<test>").unwrap();
        let (resolved, _) = resolve_module(
            "<test>".to_owned(),
            &prep.python_source,
            &prep.stripped,
            &module,
        );
        check_module("<test>", &prep.python_source, &resolved, &module)
    }

    #[test]
    fn accepts_matching_annotation() {
        let d = check("val x: int = 1\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn rejects_type_mismatch() {
        let d = check("val x: int = \"hello\"\n");
        assert!(d.has_errors());
        let msg = format!("{}", d.errors()[0]);
        assert!(msg.contains("expected `int`"), "got {}", msg);
    }

    #[test]
    fn accepts_int_into_float_target() {
        let d = check("val x: float = 1\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn rejects_none_in_non_nullable() {
        let d = check("val x: int = None\n");
        assert!(d.has_errors());
    }

    #[test]
    fn accepts_none_in_nullable() {
        let d = check("val x: int | None = None\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn optional_sugar_accepted() {
        let d = check("val x: int? = None\n");
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

val r: int = add(1)
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

val r: int = add(1, \"x\")
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

val p: Point = Point()
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn not_returns_bool_regardless_of_operand() {
        // `not x` on a non-bool operand still has type bool.
        let d = check("val flag: bool = not 1\n");
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

val s: Shape = Circle()
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

val s: Shape = Circle()

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

val s: Shape = Circle()

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

val s: Shape = Circle()

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

val s: Shape = Circle()

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
val x: int = 1

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

val s: Shape = Circle()

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

val s: Shape = Circle()

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

val x: Shape? = Circle()
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    // ── Result[T, E] type tests ───────────────────────────────────────────────

    #[test]
    fn ok_assignable_to_result() {
        let src = "\
val r: Result[int, str] = Ok(42)
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn err_assignable_to_result() {
        let src = "\
val r: Result[int, str] = Err(\"oops\")
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn plain_int_not_assignable_to_result() {
        let src = "\
val r: Result[int, str] = 42
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
val r: Result[int, str] = Ok(\"text\")
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
val r: Result[int, str] = Err(99)
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
    val r: Result[int, str] = first()
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

val d: Drawable = Circle()
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

val s: Drawable = Circle()
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

val c = Circle()
val ok: bool = isinstance(c, Drawable)
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

val c = Circle()
val ok: bool = isinstance(c, Circle)
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

val c = Circle()
val ok: bool = isinstance(c, Drawable)
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

val xs: list[int] = [1, 2, 3]
val n: int = first(xs)
";
        let d = check(src);
        assert!(!d.has_errors(), "{:?}", d.errors());
    }
}
