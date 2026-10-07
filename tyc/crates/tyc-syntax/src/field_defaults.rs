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
//! rules live here, in the lowest crate both already depend on: the
//! field-level ones, the class kinds that never receive `@dataclass`
//! ([`skips_dataclass_decoration`]), which classes the desugar reaches at
//! all and which names their defaults see ([`ClassDefaultScopes`]). Only the
//! `plain class` / `class!` markers come to each consumer its own way; both
//! test them with [`marker_covers`].

use ruff_python_ast::visitor::{walk_expr, walk_pattern, walk_stmt, Visitor};
use ruff_python_ast::{
    Decorator, ExceptHandler, Expr, Pattern, Stmt, StmtClassDef, StmtFunctionDef,
};
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
/// by the field's own annotation, or by the binding of the name the default
/// reads (`module_mutable_names` is [`ClassDefaultScopes::mutable_names`] for
/// the class, or the module's own names) — the builtin to copy it with.
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

/// Return `true` if `starts` holds an offset in the half-open range
/// `[class_start, name_start)`: a `plain` / `class!` marker on this class's
/// own `class` line. Ruff starts a decorated class's range at its first `@`,
/// and a nested class's `class` keyword lies past `name_start`, so the window
/// picks out exactly this declaration.
pub fn marker_covers(starts: &[u32], class_start: u32, name_start: u32) -> bool {
    starts.partition_point(|&off| off < class_start)
        != starts.partition_point(|&off| off < name_start)
}

// ── Class kinds the desugar never decorates ─────────────────────────────

/// A base's trailing identifier: `Enum`, `enum.Enum` and `Generic[T]`'s
/// `Generic`.
pub fn base_last_segment(base: &Expr) -> Option<&str> {
    match base {
        Expr::Name(n) => Some(n.id.as_str()),
        Expr::Attribute(a) => Some(a.attr.as_str()),
        Expr::Subscript(s) => base_last_segment(&s.value),
        _ => None,
    }
}

/// `true` if `c` inherits directly from `BaseModel` (a `model`).
pub fn class_inherits_basemodel(c: &StmtClassDef) -> bool {
    c.bases()
        .iter()
        .any(|base| matches!(base, Expr::Name(n) if n.id.as_str() == "BaseModel"))
}

/// `true` if `c` inherits directly from `Protocol` (an `interface`).
pub fn class_inherits_protocol(c: &StmtClassDef) -> bool {
    c.bases()
        .iter()
        .any(|base| matches!(base, Expr::Name(n) if n.id.as_str() == "Protocol"))
}

/// A base spelled `form`, `typing.form` or `typing_extensions.form`.
fn inherits_typing_form(c: &StmtClassDef, form: &str) -> bool {
    c.bases().iter().any(|base| match base {
        Expr::Name(n) => n.id.as_str() == form,
        Expr::Attribute(a) => {
            a.attr.as_str() == form
                && matches!(
                    &*a.value,
                    Expr::Name(n)
                    if n.id.as_str() == "typing" || n.id.as_str() == "typing_extensions"
                )
        }
        _ => false,
    })
}

/// `true` if `c` inherits directly from `TypedDict`. `@dataclass(slots=True)`
/// on one raises `TypeError` at class-definition time (FINDINGS #67).
pub fn class_inherits_typed_dict(c: &StmtClassDef) -> bool {
    inherits_typing_form(c, "TypedDict")
}

/// `true` if `c` inherits directly from `NamedTuple`, whose metaclass
/// already defines `__slots__` (FINDINGS #101).
pub fn class_inherits_named_tuple(c: &StmtClassDef) -> bool {
    inherits_typing_form(c, "NamedTuple")
}

/// Stdlib base names whose subclasses should NOT receive the auto
/// `@dataclasses.dataclass(slots=True)` decoration. Adding the dataclass
/// decorator to an `Enum`/`Flag`/`ABC` subclass either silently breaks
/// semantics (enum members get rewritten into instance fields) or raises
/// `TypeError` at class-definition time.
const SKIP_DECORATION_BUILTIN_BASES: &[&str] = &[
    "Enum", "IntEnum", "StrEnum", "Flag", "IntFlag", "ABC", "ABCMeta",
];

/// Return `true` if any base of `c` matches a known non-dataclass-friendly
/// stdlib parent (`Enum`/`Flag`/`ABC` family) or a user-supplied entry in
/// `extra` (`[emit] skip-decoration-bases`). User-supplied names are matched
/// by last identifier segment so `"App"` covers both `class T(App):` and
/// `class T(textual.App):`.
pub fn class_inherits_skip_decoration_base(c: &StmtClassDef, extra: &[String]) -> bool {
    c.bases().iter().any(|base| {
        let Some(seg) = base_last_segment(base) else {
            return false;
        };
        if SKIP_DECORATION_BUILTIN_BASES.contains(&seg) {
            return true;
        }
        extra.iter().any(|name| {
            // Allow either a bare last-segment name or a dotted path
            // (last segment of the configured name is compared).
            let configured_seg = name.rsplit('.').next().unwrap_or(name.as_str());
            configured_seg == seg
        })
    })
}

/// Return `true` if the decorator list already contains any recognized form of
/// the dataclass decorator: `@dataclass`, `@dataclass(...)`,
/// `@dataclasses.dataclass` or `@dataclasses.dataclass(...)`.
pub fn has_dataclass_decorator(decorators: &[Decorator]) -> bool {
    fn is_dataclass_expr(expr: &Expr) -> bool {
        match expr {
            Expr::Name(n) => n.id.as_str() == "dataclass",
            Expr::Attribute(a) => {
                a.attr.as_str() == "dataclass"
                    && matches!(a.value.as_ref(), Expr::Name(n) if n.id.as_str() == "dataclasses")
            }
            Expr::Call(c) => is_dataclass_expr(c.func.as_ref()),
            _ => false,
        }
    }
    decorators.iter().any(|d| is_dataclass_expr(&d.expression))
}

/// Exact base names — outside the `*Error`/`*Exception`/`*Warning` naming
/// convention — that nonetheless make a class an exception subclass.
const EXACT_EXCEPTION_BASES: &[&str] = &[
    "BaseException",
    "KeyboardInterrupt",
    "SystemExit",
    "GeneratorExit",
    "StopIteration",
    "StopAsyncIteration",
];

/// Whether `name` (a base's trailing segment) marks an exception by the
/// builtin convention: a `*Error` / `*Exception` / `*Warning` suffix or an
/// exact non-suffixed builtin (`BaseException`, `KeyboardInterrupt`, …).
fn name_is_exception_base(name: &str) -> bool {
    name.ends_with("Error")
        || name.ends_with("Exception")
        || name.ends_with("Warning")
        || EXACT_EXCEPTION_BASES.contains(&name)
}

/// Builtin metaclasses: a class deriving from one is itself a metaclass.
const METACLASS_BASES: &[&str] = &["type", "ABCMeta", "EnumMeta", "EnumType"];

/// The module's classes, and which of them are exceptions or metaclasses.
#[derive(Debug, Clone, Default)]
pub struct ModuleClassKinds {
    module_classes: HashSet<String>,
    exception_classes: HashSet<String>,
    metaclasses: HashSet<String>,
    /// Name start offsets of the classes in function and class bodies that
    /// are exceptions / metaclasses through a base class their own scope or
    /// an enclosing one declares (`class Timeout(Failure)` beside
    /// `class Failure(Exception)` in the same function).
    nested_exceptions: HashSet<u32>,
    nested_metaclasses: HashSet<u32>,
}

/// Whether a class is an exception and whether it is a metaclass.
#[derive(Debug, Clone, Copy, Default)]
struct ClassKind {
    exception: bool,
    metaclass: bool,
}

/// One scope's view of its base-class names, for the nested closure.
struct ClassFrame {
    /// The classes the scope declares, by name.
    classes: HashMap<String, ClassKind>,
    /// Other names a function binds locally: a base naming one is a value of
    /// unknown class.
    opaque: HashSet<String>,
    /// Names a function declares `global`, resolved at the module.
    globals: HashSet<String>,
    /// A class body, which the scopes nested in it do not see.
    is_class_body: bool,
}

/// What a base name in a nested class resolves to.
enum BaseClass {
    /// Not bound in any scope that declares classes: a builtin or an import,
    /// judged by its name.
    External,
    Known(ClassKind),
    /// A local value, or something the closure does not follow.
    Opaque,
}

impl ModuleClassKinds {
    /// Classify every *module-level* class — descending through module-level
    /// control flow (`if`/`try`/`for`/`while`/`with`) but NOT into function or
    /// class bodies, so a function-local `class Failure(Exception):` does not
    /// taint an unrelated top-level `class Failure:` dataclass.
    ///
    /// A class is an exception when a base is an *external* (builtin or
    /// imported) name matching the exception convention (`Exception`,
    /// `ValueError`, …) or another module class that is one. A `*Error`-named
    /// base that is itself a module class is NOT assumed to be an exception:
    /// `class LexError: line: int` is a Result error-variant dataclass, so
    /// `class Detailed(LexError):` stays a dataclass too. Metaclasses are
    /// closed the same way from [`METACLASS_BASES`].
    ///
    /// The classes of each function and class body are then closed within
    /// that scope: a base naming a class the scope or an enclosing one
    /// declares takes that class's kinds (a function-local
    /// `class Timeout(Failure)` under `class Failure(Exception)`), one naming
    /// a local value takes none, and any other is judged by its name.
    pub fn collect(body: &[Stmt]) -> Self {
        let mut classes = Vec::new();
        collect_class_bases_into(body, &mut classes);
        let module_classes: HashSet<String> = classes.iter().map(|e| e.name.clone()).collect();
        let close = |seed: &dyn Fn(&str) -> bool| {
            let mut out: HashSet<String> = classes
                .iter()
                .filter(|e| {
                    e.bases
                        .iter()
                        .any(|b| !module_classes.contains(b.as_str()) && seed(b))
                })
                .map(|e| e.name.clone())
                .collect();
            loop {
                let before = out.len();
                for e in &classes {
                    if e.bases.iter().any(|b| out.contains(b.as_str())) {
                        out.insert(e.name.clone());
                    }
                }
                if out.len() == before {
                    return out;
                }
            }
        };
        let exception_classes = close(&name_is_exception_base);
        let metaclasses = close(&|b: &str| METACLASS_BASES.contains(&b));
        let module = ClassFrame {
            classes: module_classes
                .iter()
                .map(|n| {
                    let kind = ClassKind {
                        exception: exception_classes.contains(n),
                        metaclass: metaclasses.contains(n),
                    };
                    (n.clone(), kind)
                })
                .collect(),
            opaque: HashSet::new(),
            globals: HashSet::new(),
            is_class_body: false,
        };
        let mut out = Self {
            module_classes,
            exception_classes,
            metaclasses,
            nested_exceptions: HashSet::new(),
            nested_metaclasses: HashSet::new(),
        };
        out.classify_scopes_in(body, &mut vec![module]);
        out
    }

    /// Close the exception / metaclass kinds over the classes of every
    /// function and class body declared in `body` (through its control
    /// flow), each against its own scope and the enclosing ones.
    fn classify_scopes_in(&mut self, body: &[Stmt], chain: &mut Vec<ClassFrame>) {
        for stmt in body {
            match stmt {
                Stmt::FunctionDef(f) => {
                    let bindings = ScopeBindings::of_function(f);
                    self.classify_scope(&f.body, bindings, false, chain);
                }
                Stmt::ClassDef(c) => {
                    self.classify_scope(&c.body, ScopeBindings::default(), true, chain);
                }
                Stmt::If(i) => {
                    self.classify_scopes_in(&i.body, chain);
                    for clause in &i.elif_else_clauses {
                        self.classify_scopes_in(&clause.body, chain);
                    }
                }
                Stmt::For(f) => {
                    self.classify_scopes_in(&f.body, chain);
                    self.classify_scopes_in(&f.orelse, chain);
                }
                Stmt::While(w) => {
                    self.classify_scopes_in(&w.body, chain);
                    self.classify_scopes_in(&w.orelse, chain);
                }
                Stmt::With(w) => self.classify_scopes_in(&w.body, chain),
                Stmt::Try(t) => {
                    self.classify_scopes_in(&t.body, chain);
                    for h in &t.handlers {
                        let ExceptHandler::ExceptHandler(h) = h;
                        self.classify_scopes_in(&h.body, chain);
                    }
                    self.classify_scopes_in(&t.orelse, chain);
                    self.classify_scopes_in(&t.finalbody, chain);
                }
                Stmt::Match(m) => {
                    for case in &m.cases {
                        self.classify_scopes_in(&case.body, chain);
                    }
                }
                _ => {}
            }
        }
    }

    fn classify_scope(
        &mut self,
        body: &[Stmt],
        bindings: ScopeBindings,
        is_class_body: bool,
        chain: &mut Vec<ClassFrame>,
    ) {
        let mut entries = Vec::new();
        collect_class_bases_into(body, &mut entries);
        let own: HashSet<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        let ScopeBindings {
            kinds,
            globals,
            nonlocals,
            ..
        } = bindings;
        let opaque: HashSet<String> = kinds
            .into_keys()
            .filter(|n| !own.contains(n.as_str()) && !globals.contains(n) && !nonlocals.contains(n))
            .collect();
        // A base the scope does not declare, looked up where Python looks
        // it up: the function's own locals, the enclosing functions (class
        // bodies are skipped), the module.
        let resolve = |base: &str| -> BaseClass {
            if opaque.contains(base) {
                return BaseClass::Opaque;
            }
            let frames: Box<dyn Iterator<Item = &ClassFrame>> = if globals.contains(base) {
                Box::new(chain.iter().take(1))
            } else {
                Box::new(chain.iter().rev())
            };
            for frame in frames.filter(|f| !f.is_class_body) {
                if let Some(&kind) = frame.classes.get(base) {
                    return BaseClass::Known(kind);
                }
                if frame.opaque.contains(base) {
                    return BaseClass::Opaque;
                }
                if frame.globals.contains(base) {
                    return match chain[0].classes.get(base) {
                        Some(&kind) => BaseClass::Known(kind),
                        None => BaseClass::External,
                    };
                }
            }
            BaseClass::External
        };
        // A class that declares fields is one only through an external base
        // (as [`ModuleClassKinds::is_exception_class`]'s external-base test
        // says anyway): one rooted in a module or local class keeps its
        // `@dataclass` — positional class patterns, `==` and `repr` — as it
        // always had. Only a field-less one, whose dataclass `__init__` took
        // no arguments (`raise Timeout("slow")` raised `TypeError`), changes.
        let close = |seed: &dyn Fn(&str) -> bool, known: &dyn Fn(ClassKind) -> bool| {
            let mut out: HashSet<&str> = entries
                .iter()
                .filter(|e| {
                    e.bases.iter().any(|b| {
                        !own.contains(b.as_str())
                            && match resolve(b) {
                                BaseClass::External => seed(b),
                                BaseClass::Known(kind) => !e.has_fields && known(kind),
                                BaseClass::Opaque => false,
                            }
                    })
                })
                .map(|e| e.name.as_str())
                .collect();
            loop {
                let before = out.len();
                for e in &entries {
                    if !e.has_fields && e.bases.iter().any(|b| out.contains(b.as_str())) {
                        out.insert(e.name.as_str());
                    }
                }
                if out.len() == before {
                    return out;
                }
            }
        };
        let exceptions = close(&name_is_exception_base, &|k| k.exception);
        let metaclasses = close(&|b: &str| METACLASS_BASES.contains(&b), &|k| k.metaclass);
        let mut classes = HashMap::new();
        for e in &entries {
            let kind = ClassKind {
                exception: exceptions.contains(e.name.as_str()),
                metaclass: metaclasses.contains(e.name.as_str()),
            };
            if kind.exception {
                self.nested_exceptions.insert(e.start);
            }
            if kind.metaclass {
                self.nested_metaclasses.insert(e.start);
            }
            classes.insert(e.name.clone(), kind);
        }
        chain.push(ClassFrame {
            classes,
            opaque,
            globals,
            is_class_body,
        });
        self.classify_scopes_in(body, chain);
        chain.pop();
    }

    /// Names of every module-level class.
    pub fn module_classes(&self) -> &HashSet<String> {
        &self.module_classes
    }

    /// `c` has an external exception base — in any scope, nested classes
    /// included — or is a module class rooted in one, or a nested class
    /// rooted in one its own or an enclosing scope declares.
    pub fn is_exception_class(&self, c: &StmtClassDef) -> bool {
        self.has_external_base(c, name_is_exception_base)
            || self.exception_classes.contains(c.name.as_str())
            || self
                .nested_exceptions
                .contains(&u32::from(c.name.range.start()))
    }

    /// `c` has an external metaclass base, or is a module class rooted in one,
    /// or a nested class rooted in one its scopes declare.
    pub fn is_metaclass(&self, c: &StmtClassDef) -> bool {
        self.has_external_base(c, |b| METACLASS_BASES.contains(&b))
            || self.metaclasses.contains(c.name.as_str())
            || self
                .nested_metaclasses
                .contains(&u32::from(c.name.range.start()))
    }

    fn has_external_base(&self, c: &StmtClassDef, pred: impl Fn(&str) -> bool) -> bool {
        c.bases().iter().any(|b| {
            base_last_segment(b).is_some_and(|seg| !self.module_classes.contains(seg) && pred(seg))
        })
    }
}

/// A class declaration and the trailing segments of its bases.
struct ClassEntry {
    name: String,
    /// Offset of the class name.
    start: u32,
    bases: Vec<String>,
    /// The body declares an annotated field.
    has_fields: bool,
}

/// Every class def in one scope's `body`, through its control flow but not
/// into nested function or class bodies.
fn collect_class_bases_into(body: &[Stmt], out: &mut Vec<ClassEntry>) {
    for stmt in body {
        match stmt {
            Stmt::ClassDef(c) => {
                let bases: Vec<String> = c
                    .bases()
                    .iter()
                    .filter_map(|b| base_last_segment(b).map(|s| s.to_owned()))
                    .collect();
                out.push(ClassEntry {
                    name: c.name.as_str().to_owned(),
                    start: u32::from(c.name.range.start()),
                    bases,
                    has_fields: c.body.iter().any(|s| {
                        matches!(s, Stmt::AnnAssign(a) if matches!(a.target.as_ref(), Expr::Name(_)))
                    }),
                });
            }
            Stmt::If(i) => {
                collect_class_bases_into(&i.body, out);
                for clause in &i.elif_else_clauses {
                    collect_class_bases_into(&clause.body, out);
                }
            }
            Stmt::For(f) => {
                collect_class_bases_into(&f.body, out);
                collect_class_bases_into(&f.orelse, out);
            }
            Stmt::While(w) => {
                collect_class_bases_into(&w.body, out);
                collect_class_bases_into(&w.orelse, out);
            }
            Stmt::With(w) => collect_class_bases_into(&w.body, out),
            Stmt::Try(t) => {
                collect_class_bases_into(&t.body, out);
                for h in &t.handlers {
                    let ExceptHandler::ExceptHandler(h) = h;
                    collect_class_bases_into(&h.body, out);
                }
                collect_class_bases_into(&t.orelse, out);
                collect_class_bases_into(&t.finalbody, out);
            }
            _ => {}
        }
    }
}

/// Whether a class of `c`'s kind is emitted without `@dataclass` whatever
/// its `plain` / `class!` marker says: a `model`, an `interface`, a
/// `TypedDict` / `NamedTuple`, an `Enum` / `ABC` (or configured
/// `skip_decoration_bases`) subclass, an exception or a metaclass, an
/// explicit `@dataclass`, and the `impl` / `lazy import` pseudo-classes.
/// Exceptions and metaclasses lower like `class!` instead: the default is
/// passed through a synthesised `__init__` as written.
pub fn skips_dataclass_decoration(
    c: &StmtClassDef,
    kinds: &ModuleClassKinds,
    skip_decoration_bases: &[String],
) -> bool {
    let name = c.name.as_str();
    class_inherits_basemodel(c)
        // `interface` lowers to `class X(Protocol):`; the runtime Protocol
        // behaviour conflicts with dataclass field collection.
        || class_inherits_protocol(c)
        || class_inherits_typed_dict(c)
        || class_inherits_named_tuple(c)
        // `impl` stubs are merged into their target class later.
        || name.starts_with("__typhon_impl_")
        // A `lazy import` proxy has its own `__slots__` and `__init__`.
        || name.starts_with("__TyphonLazy_")
        || class_inherits_skip_decoration_base(c, skip_decoration_bases)
        // A dataclass `__init__` would shadow `BaseException.__init__`.
        || kinds.is_exception_class(c)
        // One would replace `type.__init__` (W7-11).
        || kinds.is_metaclass(c)
        || has_dataclass_decorator(&c.decorator_list)
}

// ── Which classes the desugar reaches, and what their defaults read ──────

/// Every class the desugar's class walk visits, with the `list` / `dict` /
/// `set` names its field defaults read. The walk descends through `def` and
/// `class` bodies only, so a class under an `if` / `for` / `with` / `try` /
/// `match` is emitted exactly as written (no `@dataclass`, no copy).
///
/// A default's factory lambda resolves a name through the enclosing
/// *functions* (class bodies are skipped) before the module, so a function
/// local shadows the module binding: one evidently bound to a `list` / `dict`
/// / `set` supplies that kind, one evidently bound to something else
/// (`let BASE: tuple[int, ...] = …`) removes it, and one whose kind is not
/// evident takes the module's answer, as before locals were looked at. A
/// local's kind covers what nested functions assign to it through
/// `nonlocal`; a name a function declares `global` reads the module's
/// binding again.
#[derive(Debug, Clone, Default)]
pub struct ClassDefaultScopes {
    /// Index 0 is the module's own names; one more per function that
    /// changes them.
    scopes: Vec<HashMap<String, &'static str>>,
    /// Class name start offset → its index in `scopes`.
    classes: HashMap<u32, usize>,
}

impl ClassDefaultScopes {
    pub fn collect(body: &[Stmt]) -> Self {
        let mut out = Self {
            scopes: vec![collect_module_mutable_names(body)],
            classes: HashMap::new(),
        };
        out.walk(body, 0);
        out
    }

    /// The names the field defaults of the class whose name starts at
    /// `class_name_start` read; `None` when the desugar never reaches it.
    pub fn mutable_names(&self, class_name_start: u32) -> Option<&HashMap<String, &'static str>> {
        self.classes
            .get(&class_name_start)
            .map(|&i| &self.scopes[i])
    }

    fn walk(&mut self, body: &[Stmt], scope: usize) {
        for stmt in body {
            match stmt {
                Stmt::ClassDef(c) => {
                    self.classes.insert(u32::from(c.name.range.start()), scope);
                    self.walk(&c.body, scope);
                }
                Stmt::FunctionDef(f) => {
                    let mut names = self.scopes[scope].clone();
                    let (locals, globals) = function_local_kinds(f);
                    // A local whose kind is not evident (a mix of kinds, or a
                    // value that says nothing) takes the module's answer, as
                    // every class did before the locals were looked at — not
                    // an enclosing function's list local of the same name,
                    // which it shadows. A `global` name reads the module's
                    // binding too, past any enclosing function's local — in
                    // this function and in the scopes nested in it.
                    let mut from_module = globals;
                    for (name, kind) in locals {
                        match kind {
                            LocalKind::Mutable(k) => {
                                names.insert(name, k);
                            }
                            LocalKind::Other => {
                                names.remove(&name);
                            }
                            LocalKind::Unknown => {
                                from_module.insert(name);
                            }
                        }
                    }
                    for name in from_module {
                        match self.scopes[0].get(&name) {
                            Some(&k) => names.insert(name, k),
                            None => names.remove(&name),
                        };
                    }
                    let inner = if names == self.scopes[scope] {
                        scope
                    } else {
                        self.scopes.push(names);
                        self.scopes.len() - 1
                    };
                    self.walk(&f.body, inner);
                }
                _ => {}
            }
        }
    }
}

/// What a function-local binding evidently holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalKind {
    Mutable(&'static str),
    /// Evidently not a `list` / `dict` / `set`.
    Other,
    Unknown,
}

impl LocalKind {
    fn join(self, other: Self) -> Self {
        if self == other {
            self
        } else {
            Self::Unknown
        }
    }
}

/// An annotation or value evidently of an immutable builtin.
fn is_immutable_annotation(annotation: &Expr) -> bool {
    let head = match annotation {
        Expr::Subscript(s) => s.value.as_ref(),
        other => other,
    };
    let name = match head {
        Expr::Name(n) => n.id.as_str(),
        Expr::Attribute(a) => a.attr.as_str(),
        Expr::NoneLiteral(_) => return true,
        _ => return false,
    };
    matches!(
        name,
        "tuple"
            | "Tuple"
            | "frozenset"
            | "FrozenSet"
            | "frozendict"
            | "str"
            | "bytes"
            | "int"
            | "float"
            | "complex"
            | "bool"
    )
}

fn is_immutable_value(value: &Expr) -> bool {
    match value {
        Expr::Tuple(_)
        | Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::NumberLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NoneLiteral(_) => true,
        Expr::Call(c) => matches!(
            c.func.as_ref(),
            Expr::Name(n) if matches!(n.id.as_str(), "tuple" | "frozenset" | "frozendict")
        ),
        _ => false,
    }
}

fn binding_kind(annotation: Option<&Expr>, value: Option<&Expr>) -> LocalKind {
    if let Some(k) = annotation.and_then(mutable_builtin_of_annotation) {
        return LocalKind::Mutable(k);
    }
    if annotation.is_some_and(is_immutable_annotation) {
        return LocalKind::Other;
    }
    match value {
        Some(v) => match mutable_builtin_of_value(v) {
            Some(k) => LocalKind::Mutable(k),
            None if is_immutable_value(v) => LocalKind::Other,
            None => LocalKind::Unknown,
        },
        None => LocalKind::Unknown,
    }
}

/// What one scope binds, as far as the field defaults of the classes in it
/// care.
#[derive(Default)]
struct ScopeBindings {
    /// Every name the scope's own statements bind — parameters, assignment,
    /// `for`, `with`, `except`, import, `del`, pattern-capture and walrus
    /// targets, nested `def` / `class` names, and the names it declares
    /// `global` / `nonlocal` and then assigns — with what each evidently
    /// holds across all those bindings.
    kinds: HashMap<String, LocalKind>,
    globals: HashSet<String>,
    nonlocals: HashSet<String>,
    /// What the scope's nested functions and classes assign through
    /// `nonlocal` to names they do not bind themselves.
    nested_writes: HashMap<String, LocalKind>,
}

fn join_into(map: &mut HashMap<String, LocalKind>, name: &str, kind: LocalKind) {
    map.entry(name.to_owned())
        .and_modify(|k| *k = k.join(kind))
        .or_insert(kind);
}

impl ScopeBindings {
    fn of_function(f: &StmtFunctionDef) -> Self {
        let mut out = Self::default();
        let params = &f.parameters;
        for p in params.iter_non_variadic_params() {
            let kind = binding_kind(p.parameter.annotation.as_deref(), None);
            out.bind(p.parameter.name.as_str(), kind);
        }
        for p in params.vararg.iter().chain(params.kwarg.iter()) {
            out.bind(p.name.as_str(), LocalKind::Unknown);
        }
        for s in &f.body {
            out.visit_stmt(s);
        }
        out
    }

    fn of_class_body(body: &[Stmt]) -> Self {
        let mut out = Self::default();
        for s in body {
            out.visit_stmt(s);
        }
        out
    }

    /// What a nested `def` (`is_function`) or class body with these bindings
    /// assigns to an enclosing function's names: its own `nonlocal` names,
    /// plus whatever its nested scopes write that it does not bind itself (a
    /// class body binds nothing they can see).
    fn escaping_writes(self, is_function: bool) -> HashMap<String, LocalKind> {
        let mut out = HashMap::new();
        for name in &self.nonlocals {
            if let Some(&k) = self.kinds.get(name) {
                join_into(&mut out, name, k);
            }
        }
        for (name, k) in self.nested_writes {
            let absorbed = is_function
                && (self.globals.contains(&name)
                    || (self.kinds.contains_key(&name) && !self.nonlocals.contains(&name)));
            if !absorbed {
                join_into(&mut out, &name, k);
            }
        }
        out
    }

    fn bind(&mut self, name: &str, kind: LocalKind) {
        join_into(&mut self.kinds, name, kind);
    }

    fn bind_target(&mut self, target: &Expr) {
        match target {
            Expr::Name(n) => self.bind(n.id.as_str(), LocalKind::Unknown),
            Expr::Tuple(t) => t.elts.iter().for_each(|e| self.bind_target(e)),
            Expr::List(l) => l.elts.iter().for_each(|e| self.bind_target(e)),
            Expr::Starred(s) => self.bind_target(&s.value),
            _ => {}
        }
    }

    fn absorb_nested(&mut self, writes: HashMap<String, LocalKind>) {
        for (name, k) in writes {
            join_into(&mut self.nested_writes, &name, k);
        }
    }
}

impl<'a> Visitor<'a> for ScopeBindings {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            // A nested scope binds only its own name here; what it assigns
            // through `nonlocal` is collected separately.
            Stmt::FunctionDef(d) => {
                self.bind(d.name.as_str(), LocalKind::Unknown);
                let writes = Self::of_function(d).escaping_writes(true);
                return self.absorb_nested(writes);
            }
            Stmt::ClassDef(d) => {
                self.bind(d.name.as_str(), LocalKind::Unknown);
                let writes = Self::of_class_body(&d.body).escaping_writes(false);
                return self.absorb_nested(writes);
            }
            Stmt::Assign(a) => match a.targets.as_slice() {
                [Expr::Name(n)] => self.bind(n.id.as_str(), binding_kind(None, Some(&a.value))),
                targets => targets.iter().for_each(|t| self.bind_target(t)),
            },
            Stmt::AnnAssign(a) => {
                if let Expr::Name(n) = a.target.as_ref() {
                    self.bind(
                        n.id.as_str(),
                        binding_kind(Some(&a.annotation), a.value.as_deref()),
                    );
                }
            }
            // `+=` keeps the value's kind.
            Stmt::AugAssign(_) => {}
            Stmt::For(s) => self.bind_target(&s.target),
            Stmt::With(w) => {
                for item in &w.items {
                    if let Some(v) = &item.optional_vars {
                        self.bind_target(v);
                    }
                }
            }
            Stmt::Delete(d) => d.targets.iter().for_each(|t| self.bind_target(t)),
            Stmt::Import(i) => {
                for a in &i.names {
                    let bound = match &a.asname {
                        Some(alias) => alias.as_str(),
                        None => a.name.as_str().split('.').next().unwrap_or_default(),
                    };
                    self.bind(bound, LocalKind::Unknown);
                }
            }
            Stmt::ImportFrom(i) => {
                for a in &i.names {
                    self.bind(
                        a.asname.as_ref().unwrap_or(&a.name).as_str(),
                        LocalKind::Unknown,
                    );
                }
            }
            Stmt::Global(g) => self.globals.extend(g.names.iter().map(|n| n.to_string())),
            Stmt::Nonlocal(g) => self.nonlocals.extend(g.names.iter().map(|n| n.to_string())),
            Stmt::TypeAlias(t) => self.bind_target(&t.name),
            Stmt::Try(t) => {
                for h in &t.handlers {
                    let ExceptHandler::ExceptHandler(h) = h;
                    if let Some(name) = &h.name {
                        self.bind(name.as_str(), LocalKind::Unknown);
                    }
                }
            }
            _ => {}
        }
        walk_stmt(self, stmt);
    }
    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            // A lambda's walrus binds in the lambda.
            Expr::Lambda(_) => {}
            Expr::Named(n) => {
                self.bind_target(&n.target);
                walk_expr(self, expr);
            }
            _ => walk_expr(self, expr),
        }
    }
    fn visit_pattern(&mut self, pattern: &'a Pattern) {
        let name = match pattern {
            Pattern::MatchAs(p) => p.name.as_ref(),
            Pattern::MatchStar(p) => p.name.as_ref(),
            Pattern::MatchMapping(p) => p.rest.as_ref(),
            _ => None,
        };
        if let Some(name) = name {
            self.bind(name.as_str(), LocalKind::Unknown);
        }
        walk_pattern(self, pattern);
    }
}

/// The names `f` binds in its own scope, with what each evidently holds —
/// joined with what its nested functions assign to them through `nonlocal`
/// — and the names it declares `global`, which read the module's binding.
/// Its `nonlocal` names are the enclosing function's.
fn function_local_kinds(f: &StmtFunctionDef) -> (HashMap<String, LocalKind>, HashSet<String>) {
    let ScopeBindings {
        mut kinds,
        globals,
        nonlocals,
        nested_writes,
    } = ScopeBindings::of_function(f);
    kinds.retain(|name, _| !globals.contains(name) && !nonlocals.contains(name));
    for (name, k) in nested_writes {
        if let Some(own) = kinds.get_mut(&name) {
            *own = own.join(k);
        }
    }
    (kinds, globals)
}
