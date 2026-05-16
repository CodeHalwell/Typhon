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
    Function {
        params: Vec<Type>,
        ret: Box<Type>,
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
                let kept: Vec<Type> = xs.iter().filter(|t| !matches!(t, Type::None)).cloned().collect();
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
            Type::Function { params, ret } => {
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
            an == bn && aa.len() == bb.len() && aa.iter().zip(bb).all(|(x, y)| assignable(x, y))
        }
        (a, b) => a == b,
    }
}

/// Translate an annotation expression into a [`Type`].
///
/// `classes` is the set of class names declared in the enclosing module so
/// we can resolve nominal references.
pub fn type_from_annotation(expr: &Expr<TextRange>, classes: &[String]) -> Type {
    match expr {
        Expr::Name(n) => match n.id.as_str() {
            "int" => Type::Int,
            "str" => Type::Str,
            "bool" => Type::Bool,
            "float" => Type::Float,
            "bytes" => Type::Bytes,
            "None" => Type::None,
            "Any" => Type::Any,
            other if classes.iter().any(|c| c == other) => Type::Class(other.to_owned()),
            // Unknown but treat as nominal class (may be imported).
            other => Type::Class(other.to_owned()),
        },
        Expr::BinOp(b) if matches!(b.op, rustpython_ast::Operator::BitOr) => {
            let left = type_from_annotation(&b.left, classes);
            let right = type_from_annotation(&b.right, classes);
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
                return Type::optional(type_from_annotation(&s.slice, classes));
            }
            // Union[A, B, ...] / typing.Union[...]
            if head == "Union" {
                if let Expr::Tuple(t) = s.slice.as_ref() {
                    let args: Vec<Type> = t.elts.iter().map(|e| type_from_annotation(e, classes)).collect();
                    return Type::union_of(args);
                }
                return type_from_annotation(&s.slice, classes);
            }
            // list[int], dict[str, int], tuple[int, str, ...]
            let args: Vec<Type> = match s.slice.as_ref() {
                Expr::Tuple(t) => t.elts.iter().map(|e| type_from_annotation(e, classes)).collect(),
                other => vec![type_from_annotation(other, classes)],
            };
            Type::Generic(head, args)
        }
        Expr::Constant(c) if matches!(c.value, Constant::None) => Type::None,
        _ => Type::Unknown,
    }
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
    env: TypeEnv,
    diagnostics: Diagnostics,
    /// Return type of the function whose body we are currently checking
    /// (None at module scope).
    current_return: Option<Type>,
}

impl<'a> Checker<'a> {
    fn new(path: String, source: &'a str, resolved: &'a ResolvedModule) -> Self {
        Self {
            path,
            source,
            resolved,
            classes: Vec::new(),
            function_signatures: HashMap::new(),
            sealed_unions: HashMap::new(),
            env: TypeEnv::default(),
            diagnostics: Diagnostics::new(),
            current_return: None,
        }
    }

    /// Assignment compatibility check that accounts for sealed-union subtyping.
    ///
    /// Extends the module-level [`assignable`] function with two additional rules:
    ///
    /// 1. **Variant → sealed union**: `Circle` is assignable to `Shape` when
    ///    `type Shape = Circle | Rectangle | ...` is declared.
    /// 2. **Union interception**: when `expected` is a `Union`, retry each variant
    ///    with `is_assignable` so that sealed-union knowledge is available inside
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
        }
        // For Union expected types (e.g. `Shape | None`), `assignable` recurses
        // using only the base rules. Re-check each variant here so sealed-union
        // knowledge is available in the recursive call.
        if let Type::Union(variants) = expected {
            return variants.iter().any(|v| self.is_assignable(v, actual));
        }
        false
    }

    fn mismatch(&mut self, expected: &Type, actual: &Type, span: (usize, usize)) {
        let length = span.1.saturating_sub(span.0).max(1);
        self.diagnostics.push_error(TycError::type_mismatch(
            expected.display(),
            actual.display(),
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

    fn non_exhaustive_match(
        &mut self,
        union_name: &str,
        missing: &str,
        span: (usize, usize),
    ) {
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
        for stmt in &m.body {
            check_stmt(&mut c, stmt);
        }
        c.env.leave();
    }

    c.diagnostics
}

fn collect_classes_and_functions(c: &mut Checker, body: &[Stmt<TextRange>]) {
    // First pass: collect class names and sealed union declarations so that
    // type_from_annotation can resolve them and match exhaustiveness can be
    // checked.
    for stmt in body {
        match stmt {
            Stmt::ClassDef(cd) => {
                c.classes.push(cd.name.as_str().to_owned());
            }
            Stmt::TypeAlias(ta) => {
                if let Expr::Name(n) = ta.name.as_ref() {
                    let union_name = n.id.as_str().to_owned();
                    if let Some(variants) = extract_sealed_union_variants(&ta.value) {
                        c.classes.push(union_name.clone());
                        c.sealed_unions.insert(union_name, variants);
                    }
                }
            }
            _ => {}
        }
    }
    // Now record function signatures.
    let classes = c.classes.clone();
    for stmt in body {
        match stmt {
            Stmt::FunctionDef(f) => {
                let sig = function_signature(&classes, f.args.as_ref(), f.returns.as_deref());
                c.function_signatures
                    .insert(f.name.as_str().to_owned(), sig);
            }
            Stmt::AsyncFunctionDef(f) => {
                let sig = function_signature(&classes, f.args.as_ref(), f.returns.as_deref());
                c.function_signatures
                    .insert(f.name.as_str().to_owned(), sig);
            }
            _ => {}
        }
    }
}

fn function_signature(
    classes: &[String],
    args: &rustpython_ast::Arguments<TextRange>,
    returns: Option<&Expr<TextRange>>,
) -> Type {
    let mut params = Vec::new();
    let all = args
        .posonlyargs
        .iter()
        .chain(args.args.iter())
        .chain(args.kwonlyargs.iter());
    for arg in all {
        let t = match &arg.def.annotation {
            Some(ann) => type_from_annotation(ann, classes),
            None => Type::Unknown,
        };
        params.push(t);
    }
    let ret = match returns {
        Some(r) => type_from_annotation(r, classes),
        None => Type::Unknown,
    };
    Type::Function {
        params,
        ret: Box::new(ret),
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
        Stmt::FunctionDef(f) => check_function(c, f.name.as_str(), &f.args, &f.body, f.returns.as_deref()),
        Stmt::AsyncFunctionDef(f) => check_function(c, f.name.as_str(), &f.args, &f.body, f.returns.as_deref()),
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
                    if !matches!(expected, Type::Unknown) && !c.is_assignable(&expected, &Type::None) {
                        let span = (
                            ret.range.start().to_usize(),
                            ret.range.end().to_usize(),
                        );
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
) {
    let _ = name;
    let classes = c.classes.clone();
    let ret_type = match returns {
        Some(r) => type_from_annotation(r, &classes),
        None => Type::Unknown,
    };

    let saved_return = c.current_return.replace(ret_type);
    c.env.enter();

    // Declare parameters with their annotation types.
    let all = args
        .posonlyargs
        .iter()
        .chain(args.args.iter())
        .chain(args.kwonlyargs.iter());
    for arg in all {
        let t = match &arg.def.annotation {
            Some(ann) => type_from_annotation(ann, &classes),
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
                                let want_none = if negate { !positive_match } else { positive_match };
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
                (Type::Str, Type::Str)
                    if matches!(b.op, rustpython_ast::Operator::Add) =>
                {
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
            let func_type = infer_expr(c, &call.func);
            let call_span = (
                call.range.start().to_usize(),
                call.range.end().to_usize(),
            );

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
                Type::Function { params, ret } => {
                    // Argument count check (positional only — conservative).
                    if call.args.len() != params.len() {
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
                            let span = (
                                arg.range().start().to_usize(),
                                arg.range().end().to_usize(),
                            );
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
        let (resolved, _) =
            resolve_module("<test>".to_owned(), &prep.python_source, &prep.stripped, &module);
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
        assert!(d.has_errors(), "guarded arm must not satisfy exhaustiveness");
        let msg = format!("{}", d.errors()[0]);
        assert!(msg.contains("Circle"), "error should name Circle, got: {msg}");
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
}
