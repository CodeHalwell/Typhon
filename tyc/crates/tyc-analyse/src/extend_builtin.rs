//! Extension methods on Python built-ins.
//!
//! Pipeline:
//!
//! 1. **Preprocess** has already lowered `extend BUILTIN:` blocks to a
//!    sentinel class shape `class __typhon_builtin_ext_BUILTIN(object): …`.
//! 2. [`extract_builtin_extensions`] walks the module body, finds those
//!    sentinel classes, extracts each method to a module-level free
//!    function `__typhon_ext_BUILTIN__METHOD__`, removes the class, and
//!    builds a registry (`BUILTIN → method-name → free-fn-name`).
//! 3. [`rewrite_builtin_extension_calls`] uses the registry plus a static
//!    view of the module's types to turn `x.METHOD(args)` into
//!    `__typhon_ext_BUILTIN__METHOD__(x, args)` whenever the receiver `x`
//!    is statically known to be one of the registered built-ins.
//!
//! The rewrite is *strictly opt-in by static type*. When the receiver
//! cannot be proven to be the matching built-in, the call is left as a
//! native attribute access — which raises `AttributeError` at runtime,
//! matching Python's existing semantics for missing methods. The
//! conservative bias keeps the rewrite from corrupting legitimate
//! attribute accesses on user-defined types that happen to share a
//! method name with a registered extension.
//!
//! # What "statically known" covers
//!
//! The receiver's type is read off declarations, never guessed:
//!
//! - an annotated name — a parameter, a `let x: T` / `mut x: T` binding,
//!   or a module-level annotated constant (also when read from inside a
//!   function, since Python resolves the global and its declared type is
//!   stable);
//! - an unannotated `let x = EXPR` whose initialiser has an evident type
//!   (the checker fixes a binding's type at its first assignment, so the
//!   refinement is stable for straight-line code; it is dropped again at
//!   every branch join and loop head where the name is reassigned);
//! - a literal, an f-string, or a display (`"a b".slug()`, `[…]`, `{…}`);
//! - a field of a class declared in the module (`self.title` inside an
//!   `impl Post:`, `post.title` on a `post: Post`), including inherited
//!   fields, `@property` getters, and generic fields with the class's type
//!   parameters substituted (`b: Box[str]` → `b.value` is `str`);
//! - a call to a function or method with a declared return type — a
//!   same-module `def`, an `impl` method, a `@staticmethod`, a constructor
//!   (`Post(...)`), an imported function / class whose facts the caller
//!   supplies through [`TypeFacts`], and a module-qualified call
//!   (`textutil.make()`); an `async def` is *not* a value of its declared
//!   return type until awaited, so only `(await f()).slug()` types;
//! - a chained extension call (`x.slug().shout()`), typed through the
//!   lifted free function's declared return;
//! - a subscript on a parametric container (`xs[0]` with `xs: list[str]`,
//!   `d[k]` with `d: dict[str, str]`, `t[1]` on a fixed-arity tuple);
//! - a `for` target / comprehension variable over a parametric iterable;
//! - a chain of type-preserving builtin methods (`t.strip().lower()`),
//!   plus a small table of element-producing ones (`s.split()[0]`);
//! - a few builtin operators (`(a + " " + b).slug()`, `s * 3`, `s % x`).
//!
//! A `T?` (`T | None`) receiver is treated as `T`: the checker only
//! accepts a method call on a nullable value once it has been narrowed,
//! and `None.method()` is a guaranteed `AttributeError` either way.
//!
//! The environment is flow-sensitive for *inferred* bindings only:
//! declared types are stable for the whole scope (the checker enforces
//! that every later assignment conforms), while an inferred refinement
//! made inside a nested block is discarded at the block's end, and a
//! loop drops the refinement of every name its body reassigns before
//! entering. Function scopes inherit the enclosing scope's declared
//! names (not its refinements) and any local binding masks an inherited
//! or module-level name, so a shadowing rebinding can never trigger a
//! false-positive rewrite. Calls are not assumed to mutate a global's
//! type — the checker makes every binding type-stable, so the only way a
//! call could invalidate a refinement is through a `mut` global declared
//! as a union of two extended builtins, a shape the checker itself widens
//! back to the union (and rejects the call on) across the same call.

use std::collections::{HashMap, HashSet};

use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_ast::{
    name::Name, AtomicNodeIndex, Comprehension, Expr, ExprCall, ExprName, ModModule, Operator,
    Pattern, Stmt, StmtClassDef, StmtFunctionDef, TypeParam, UnaryOp,
};
use ruff_text_size::TextRange;

/// Marker prefix preprocess uses when lowering `extend BUILTIN:` for one
/// of the recognised Python built-in types.
const STUB_PREFIX: &str = "__typhon_builtin_ext_";

/// Marker prefix preprocess uses when lowering `impl ClassName:` /
/// `extend ClassName:` to a pseudo-class that desugar later merges into
/// the target class.
const IMPL_PREFIX: &str = "__typhon_impl_";

/// Placeholder head for a type the pass cannot see (an element of a
/// tuple it otherwise knows the shape of, or a `Callable`'s parameter
/// list). Never matches a registered builtin.
const UNKNOWN_HEAD: &str = "?";

/// Annotate the lifted free function's `self` parameter with the
/// extended builtin's type (FINDINGS #54). Only fires when the first
/// positional parameter is currently unannotated and named `self`; an
/// explicit annotation the user already wrote wins. The annotation
/// node is synthesised with a zero-length `TextRange` so source-map
/// emission inherits the surrounding offset (matching how other
/// desugar passes synthesise AST nodes).
fn annotate_self_param_with_builtin(f: &mut ruff_python_ast::StmtFunctionDef, builtin: &str) {
    let target = f
        .parameters
        .posonlyargs
        .first_mut()
        .or_else(|| f.parameters.args.first_mut());
    let Some(target) = target else { return };
    if target.parameter.name.as_str() != "self" {
        return;
    }
    if target.parameter.annotation.is_some() {
        return;
    }
    target.parameter.annotation = Some(Box::new(Expr::Name(ExprName {
        range: ruff_text_size::TextRange::default(),
        node_index: AtomicNodeIndex::NONE,
        id: Name::new(builtin),
        ctx: ruff_python_ast::ExprContext::Load,
    })));
}

/// Free-function naming convention: `__typhon_ext_<TYPE>__<METHOD>__`.
///
/// The trailing `__` matters: CPython name-mangles any identifier that
/// starts with two underscores and does *not* end with two, wherever it
/// appears inside a class body — so a call rewritten inside `impl Post:`
/// (`self.title.slug()` → `__typhon_ext_str__slug__(self.title)`) became
/// `_Post__typhon_ext_str__slug` and raised `NameError`. A dunder-shaped
/// name is exempt from mangling.
pub fn free_fn_name(ty: &str, method: &str) -> String {
    format!("__typhon_ext_{ty}__{method}__")
}

/// Maps `type-name → method-name → free-function-name`.
pub type ExtensionRegistry = HashMap<String, HashMap<String, String>>;

/// Summary of an extraction pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtensionExtractionStats {
    /// Number of `extend BUILTIN:` blocks consumed.
    pub blocks: usize,
    /// Total number of methods promoted to free functions.
    pub methods: usize,
}

/// Walk `module` and replace every `class __typhon_builtin_ext_BUILTIN(object):`
/// stub with the equivalent set of module-level free-function definitions.
///
/// Returns the extracted registry plus a small statistics struct for
/// diagnostics.  Subsequent passes use the registry to rewrite call sites.
pub fn extract_builtin_extensions(
    module: &mut ModModule,
) -> (ExtensionRegistry, ExtensionExtractionStats) {
    let mut registry: ExtensionRegistry = HashMap::new();
    let mut stats = ExtensionExtractionStats::default();

    let original = std::mem::take(&mut module.body);
    let mut rebuilt: Vec<Stmt> = Vec::with_capacity(original.len());
    for stmt in original {
        if let Stmt::ClassDef(c) = &stmt {
            if let Some(builtin) = c.name.as_str().strip_prefix(STUB_PREFIX) {
                let builtin = builtin.to_owned();
                stats.blocks += 1;
                let entry = registry.entry(builtin.clone()).or_default();
                for member in &c.body {
                    if let Stmt::FunctionDef(f) = member {
                        let mut promoted = f.clone();
                        let new_name = free_fn_name(&builtin, f.name.as_str());
                        promoted.name = ruff_python_ast::Identifier {
                            range: f.name.range,
                            node_index: AtomicNodeIndex::NONE,
                            id: Name::new(&new_name),
                        };
                        // Annotate the receiver (`self`) with the
                        // builtin's type so the lifted free function
                        // satisfies Rule 1 (FINDINGS #54) and `tyc ty`
                        // / pyright / mypy can type-check the body.
                        // The annotation is set only on the first
                        // positional-or-keyword parameter if it is
                        // currently unannotated and named `self`; any
                        // explicit annotation the user already wrote
                        // wins.
                        annotate_self_param_with_builtin(&mut promoted, &builtin);
                        entry.insert(f.name.as_str().to_owned(), new_name);
                        rebuilt.push(Stmt::FunctionDef(promoted));
                        stats.methods += 1;
                    }
                    // Non-function members (docstrings, class-level
                    // assignments) are silently dropped — `extend
                    // BUILTIN:` is for methods only, mirroring the
                    // user-class `impl`-merge contract.
                }
                continue;
            }
        }
        rebuilt.push(stmt);
    }
    module.body = rebuilt;
    (registry, stats)
}

// ── static types and type facts ─────────────────────────────────────────────

/// A statically evident type: a head name plus its type arguments
/// (`list[str]` is `head: "list", args: [str]`). Class heads are bare
/// names for classes declared in or imported into the module, and
/// `module.Class` for a module-qualified reference.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StaticType {
    /// The outer type name (`str`, `list`, `Post`, `textutil.Post`).
    pub head: String,
    /// Type arguments in source order; empty when unknown or absent.
    pub args: Vec<StaticType>,
}

impl StaticType {
    /// A type with no arguments.
    pub fn simple(head: impl Into<String>) -> Self {
        Self {
            head: head.into(),
            args: Vec::new(),
        }
    }

    /// A parametric type.
    pub fn with_args(head: impl Into<String>, args: Vec<StaticType>) -> Self {
        Self {
            head: head.into(),
            args,
        }
    }

    fn unknown() -> Self {
        Self::simple(UNKNOWN_HEAD)
    }

    fn is_known(&self) -> bool {
        self.head != UNKNOWN_HEAD
    }

    /// Read a type off an annotation expression. Supports bare and dotted
    /// names, the bracketed generic forms (`list[int]`, `dict[str, int]`,
    /// `tuple[str, ...]`, `Callable[[int], str]`), `Optional[X]` and the
    /// `X | None` lowering of `X?` (both read as `X`), and a quoted
    /// forward reference (`"Post"`). Any other union, and anything else
    /// the pass cannot model, is `None`.
    pub fn from_annotation(ann: &Expr) -> Option<Self> {
        match ann {
            Expr::Name(n) => Some(Self::simple(n.id.as_str())),
            Expr::NoneLiteral(_) => Some(Self::simple("None")),
            Expr::Attribute(_) => dotted_name(ann).map(Self::simple),
            Expr::StringLiteral(s) => {
                let text = s.value.to_str();
                is_identifier(text).then(|| Self::simple(text))
            }
            Expr::Subscript(s) => {
                let head = dotted_name(&s.value)?;
                let raw: Vec<&Expr> = match s.slice.as_ref() {
                    Expr::Tuple(t) => t.elts.iter().collect(),
                    other => vec![other],
                };
                let mut args = Vec::with_capacity(raw.len());
                for a in raw {
                    match a {
                        Expr::EllipsisLiteral(_) => args.push(Self::simple("...")),
                        // A `Callable`'s parameter list; never a type.
                        Expr::List(l) => args.push(Self::with_args(
                            "parameters",
                            l.elts
                                .iter()
                                .map(|e| Self::from_annotation(e).unwrap_or_else(Self::unknown))
                                .collect(),
                        )),
                        other => match Self::from_annotation(other) {
                            Some(t) => args.push(t),
                            None => {
                                // Partial argument lists would misalign
                                // positional lookups; keep the head only.
                                args.clear();
                                break;
                            }
                        },
                    }
                }
                // `Optional[X]` reads as `X`, like the `X | None` form.
                if head.rsplit('.').next() == Some("Optional") && args.len() == 1 {
                    return args.pop();
                }
                Some(Self { head, args })
            }
            Expr::BinOp(b) if matches!(b.op, Operator::BitOr) => {
                let mut members = Vec::new();
                collect_union_members(ann, &mut members);
                let non_none: Vec<&Expr> = members
                    .into_iter()
                    .filter(|m| !matches!(m, Expr::NoneLiteral(_)))
                    .collect();
                match non_none.as_slice() {
                    [single] => Self::from_annotation(single),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

fn collect_union_members<'e>(expr: &'e Expr, out: &mut Vec<&'e Expr>) {
    match expr {
        Expr::BinOp(b) if matches!(b.op, Operator::BitOr) => {
            collect_union_members(&b.left, out);
            collect_union_members(&b.right, out);
        }
        other => out.push(other),
    }
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_alphanumeric())
}

/// `a.b.c` for a chain of attribute accesses rooted at a name.
fn dotted_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name(n) => Some(n.id.as_str().to_owned()),
        Expr::Attribute(a) => {
            let mut base = dotted_name(&a.value)?;
            base.push('.');
            base.push_str(a.attr.as_str());
            Some(base)
        }
        _ => None,
    }
}

/// Everything the rewrite pass knows about one class: its declared
/// fields (and `@property` getters), the declared return type of its
/// methods, its PEP 695 type parameters, and its base classes.
#[derive(Debug, Clone, Default)]
pub struct ClassFacts {
    /// PEP 695 type parameter names in declaration order.
    pub type_params: Vec<String>,
    /// Base class names as written on the header (`object` excluded).
    pub bases: Vec<String>,
    /// Field name → declared type. `@property` / `cached_property`
    /// getters are recorded here under their (call-free) attribute name.
    pub fields: HashMap<String, StaticType>,
    /// Instance method name → declared return type (sync methods only).
    pub methods: HashMap<String, StaticType>,
    /// `async def` instance method name → declared return type (the
    /// value an `await` of the call produces).
    pub async_methods: HashMap<String, StaticType>,
    /// `@staticmethod` / `@classmethod` name → declared return type.
    pub static_methods: HashMap<String, StaticType>,
}

/// Type facts the rewrite pass consults beyond the module it is
/// rewriting. Keyed by the *local* name the consumer module uses.
///
/// `tyc build` fills this from the project-wide shape registry for every
/// name the module imports (`from textutil import Post, make` → `classes
/// ["Post"]`, `functions["make"]`; `import textutil` → `modules
/// ["textutil"]`); the VM fills it from the sibling `.ty` files the entry
/// module imports. [`collect_module_type_facts`] produces the per-module
/// view these are assembled from.
#[derive(Debug, Clone, Default)]
pub struct TypeFacts {
    /// Free function name → declared return type (sync functions only).
    /// Lifted extension functions (`__typhon_ext_str__slug__`) are
    /// recorded here too, so a chained extension call can be typed.
    pub functions: HashMap<String, StaticType>,
    /// `async def` name → declared return type.
    pub async_functions: HashMap<String, StaticType>,
    /// Class name → its facts.
    pub classes: HashMap<String, ClassFacts>,
    /// Module alias (`import textutil` → `"textutil"`, `import a.b as c`
    /// → `"c"`) → that module's facts, for `alias.f()` / `alias.Cls`.
    pub modules: HashMap<String, TypeFacts>,
}

impl TypeFacts {
    /// `true` when nothing is recorded.
    pub fn is_empty(&self) -> bool {
        self.functions.is_empty()
            && self.async_functions.is_empty()
            && self.classes.is_empty()
            && self.modules.is_empty()
    }

    /// Merge `other` into `self`; `other` wins on a name both know. A
    /// function `other` knows to be `async` is also dropped from
    /// `self.functions` (and vice versa), so a source that cannot tell
    /// the two apart never mislabels one the AST-derived view can.
    pub fn merge(&mut self, other: TypeFacts) {
        for (name, ty) in other.functions {
            self.async_functions.remove(&name);
            self.functions.insert(name, ty);
        }
        for (name, ty) in other.async_functions {
            self.functions.remove(&name);
            self.async_functions.insert(name, ty);
        }
        self.classes.extend(other.classes);
        self.modules.extend(other.modules);
    }

    /// Copy the facts recorded under `name` into `self` under `local`
    /// (the `from M import name as local` shape).
    pub fn import_name(&mut self, source: &TypeFacts, name: &str, local: &str) {
        if let Some(ty) = source.functions.get(name) {
            self.async_functions.remove(local);
            self.functions.insert(local.to_owned(), ty.clone());
        }
        if let Some(ty) = source.async_functions.get(name) {
            self.functions.remove(local);
            self.async_functions.insert(local.to_owned(), ty.clone());
        }
        if let Some(cls) = source.classes.get(name) {
            self.classes.insert(local.to_owned(), cls.clone());
        }
    }
}

/// Derive the [`TypeFacts`] of one module from its AST: the declared
/// return type of every top-level `def` (sync and async kept apart), and
/// the fields, methods, type parameters and bases of every top-level
/// class — `impl ClassName:` / `extend ClassName:` pseudo-classes fold
/// into their target, and an `extend BUILTIN:` sentinel that has not been
/// extracted yet publishes its methods under the lifted free-function
/// names. A function rebound by a later module-level assignment is not
/// trusted.
pub fn collect_module_type_facts(module: &ModModule) -> TypeFacts {
    let mut facts = TypeFacts::default();
    let mut rebound: HashSet<&str> = HashSet::new();
    for stmt in &module.body {
        match stmt {
            Stmt::Assign(a) => {
                for t in &a.targets {
                    if let Expr::Name(n) = t {
                        rebound.insert(n.id.as_str());
                    }
                }
            }
            Stmt::AnnAssign(a) => {
                if let Expr::Name(n) = a.target.as_ref() {
                    rebound.insert(n.id.as_str());
                }
            }
            _ => {}
        }
    }
    for stmt in &module.body {
        match stmt {
            Stmt::FunctionDef(f) => {
                if rebound.contains(f.name.as_str()) {
                    continue;
                }
                let Some(ret) = declared_return(f) else {
                    continue;
                };
                if f.is_async {
                    facts
                        .async_functions
                        .insert(f.name.as_str().to_owned(), ret);
                } else {
                    facts.functions.insert(f.name.as_str().to_owned(), ret);
                }
            }
            Stmt::ClassDef(c) => {
                if let Some(builtin) = c.name.as_str().strip_prefix(STUB_PREFIX) {
                    for member in &c.body {
                        if let Stmt::FunctionDef(m) = member {
                            if m.is_async {
                                continue;
                            }
                            if let Some(ret) = declared_return(m) {
                                facts
                                    .functions
                                    .insert(free_fn_name(builtin, m.name.as_str()), ret);
                            }
                        }
                    }
                    continue;
                }
                let name = c
                    .name
                    .as_str()
                    .strip_prefix(IMPL_PREFIX)
                    .unwrap_or(c.name.as_str());
                let entry = facts.classes.entry(name.to_owned()).or_default();
                absorb_class_body(entry, c);
            }
            _ => {}
        }
    }
    facts
}

fn declared_return(f: &StmtFunctionDef) -> Option<StaticType> {
    f.returns.as_deref().and_then(StaticType::from_annotation)
}

enum MethodKind {
    Instance,
    Property,
    Static,
    /// `@overload` or a setter/deleter: the return type says nothing
    /// useful about the attribute.
    Skip,
}

fn method_kind(f: &StmtFunctionDef) -> MethodKind {
    let mut kind = MethodKind::Instance;
    for decorator in &f.decorator_list {
        let name = match &decorator.expression {
            Expr::Name(n) => n.id.as_str(),
            Expr::Attribute(a) => a.attr.as_str(),
            Expr::Call(c) => match c.func.as_ref() {
                Expr::Name(n) => n.id.as_str(),
                Expr::Attribute(a) => a.attr.as_str(),
                _ => continue,
            },
            _ => continue,
        };
        match name {
            "property" | "cached_property" | "_typhon_cached_property" => {
                kind = MethodKind::Property;
            }
            "staticmethod" | "classmethod" => kind = MethodKind::Static,
            "setter" | "deleter" | "overload" => return MethodKind::Skip,
            _ => {}
        }
    }
    kind
}

fn absorb_class_body(entry: &mut ClassFacts, c: &StmtClassDef) {
    if entry.type_params.is_empty() {
        if let Some(tp) = &c.type_params {
            for p in &tp.type_params {
                if let TypeParam::TypeVar(v) = p {
                    entry.type_params.push(v.name.as_str().to_owned());
                }
            }
        }
    }
    if let Some(args) = &c.arguments {
        for base in &args.args {
            let head = match base {
                Expr::Subscript(s) => dotted_name(&s.value),
                other => dotted_name(other),
            };
            if let Some(head) = head {
                if head != "object" && !entry.bases.contains(&head) {
                    entry.bases.push(head);
                }
            }
        }
    }
    for member in &c.body {
        match member {
            Stmt::AnnAssign(a) => {
                if let (Expr::Name(n), Some(ty)) = (
                    a.target.as_ref(),
                    StaticType::from_annotation(&a.annotation),
                ) {
                    entry.fields.insert(n.id.as_str().to_owned(), ty);
                }
            }
            Stmt::FunctionDef(m) => {
                let Some(ret) = declared_return(m) else {
                    continue;
                };
                let name = m.name.as_str().to_owned();
                match method_kind(m) {
                    MethodKind::Property => {
                        entry.fields.insert(name, ret);
                    }
                    MethodKind::Static => {
                        if !m.is_async {
                            entry.static_methods.insert(name, ret);
                        }
                    }
                    MethodKind::Instance => {
                        if m.is_async {
                            entry.async_methods.insert(name, ret);
                        } else {
                            entry.methods.insert(name, ret);
                        }
                    }
                    MethodKind::Skip => {}
                }
            }
            _ => {}
        }
    }
}

// ── the rewrite pass ────────────────────────────────────────────────────────

/// Rewrite `x.method(args)` calls into `__typhon_ext_TYPE__method__(x, args)`
/// for every receiver whose static type places it in one of the
/// registered built-in types.
///
/// Returns the number of call sites successfully rewritten.
pub fn rewrite_builtin_extension_calls(
    module: &mut ModModule,
    registry: &ExtensionRegistry,
) -> usize {
    rewrite_builtin_extension_calls_tracking(module, registry).0
}

/// Like [`rewrite_builtin_extension_calls`] but also returns the set of
/// free-function names that were actually used in rewrites. The caller can
/// use this to inject cross-module imports for extension functions defined
/// in another module. (#202)
pub fn rewrite_builtin_extension_calls_tracking(
    module: &mut ModModule,
    registry: &ExtensionRegistry,
) -> (usize, HashSet<String>) {
    rewrite_builtin_extension_calls_with_facts(module, registry, &TypeFacts::default())
}

/// Like [`rewrite_builtin_extension_calls_tracking`], with type facts about
/// the names this module imports from elsewhere (`external`), so a
/// receiver such as `post.title` on an imported `Post`, `make()` on an
/// imported function, or `textutil.make()` on an imported module types
/// the same way a same-module declaration does.
pub fn rewrite_builtin_extension_calls_with_facts(
    module: &mut ModModule,
    registry: &ExtensionRegistry,
    external: &TypeFacts,
) -> (usize, HashSet<String>) {
    if registry.is_empty() {
        return (0, HashSet::new());
    }
    let ctx = RewriteCtx {
        registry,
        local: collect_module_type_facts(module),
        external,
    };
    let mut env = Env::module(module);
    let mut walker = Walker {
        ctx: &ctx,
        rewrites: 0,
        used_fns: HashSet::new(),
    };
    let body = std::mem::take(&mut module.body);
    module.body = walker.body(body, &mut env);
    (walker.rewrites, walker.used_fns)
}

/// What one rewrite pass knows beyond the local binding environment: the
/// extension registry, the module's own declarations, and the facts the
/// caller supplied about imported names.
struct RewriteCtx<'a> {
    registry: &'a ExtensionRegistry,
    local: TypeFacts,
    external: &'a TypeFacts,
}

#[derive(Clone, Copy)]
enum Member {
    Field,
    Method,
    AsyncMethod,
    StaticMethod,
}

impl RewriteCtx<'_> {
    fn class_facts(&self, head: &str) -> Option<&ClassFacts> {
        if let Some((module, cls)) = head.rsplit_once('.') {
            return self
                .external
                .modules
                .get(module)
                .and_then(|m| m.classes.get(cls));
        }
        self.local
            .classes
            .get(head)
            .or_else(|| self.external.classes.get(head))
    }

    fn is_class(&self, head: &str) -> bool {
        self.class_facts(head).is_some()
    }

    fn function(&self, name: &str) -> Option<&StaticType> {
        self.local
            .functions
            .get(name)
            .or_else(|| self.external.functions.get(name))
    }

    fn async_function(&self, name: &str) -> Option<&StaticType> {
        self.local
            .async_functions
            .get(name)
            .or_else(|| self.external.async_functions.get(name))
    }

    fn module(&self, dotted: &str) -> Option<&TypeFacts> {
        self.external.modules.get(dotted)
    }

    /// The free function a registered extension method lowers to, when
    /// `owner` is an extended builtin declaring `method`.
    fn extension_fn(&self, owner: &StaticType, method: &str) -> Option<&String> {
        self.registry.get(&owner.head).and_then(|m| m.get(method))
    }

    /// Look `member` up on `owner`'s class and, failing that, its bases,
    /// substituting the class's type parameters with `owner.args`.
    fn class_member(&self, owner: &StaticType, member: &str, kind: Member) -> Option<StaticType> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: Vec<(String, Vec<StaticType>)> =
            vec![(owner.head.clone(), owner.args.clone())];
        while let Some((head, args)) = queue.pop() {
            if !visited.insert(head.clone()) || visited.len() > 32 {
                continue;
            }
            let Some(facts) = self.class_facts(&head) else {
                continue;
            };
            let table = match kind {
                Member::Field => &facts.fields,
                Member::Method => &facts.methods,
                Member::AsyncMethod => &facts.async_methods,
                Member::StaticMethod => &facts.static_methods,
            };
            if let Some(ty) = table.get(member) {
                return Some(substitute(ty, &facts.type_params, &args));
            }
            // A base written bare inside a module-qualified class lives
            // in that same module.
            let module_prefix = head.rsplit_once('.').map(|(m, _)| m);
            for base in facts.bases.iter().rev() {
                let base = match module_prefix {
                    Some(m) if !base.contains('.') => format!("{m}.{base}"),
                    _ => base.clone(),
                };
                queue.push((base, Vec::new()));
            }
        }
        None
    }
}

/// Replace every bare type-parameter name in `ty` with the matching
/// argument. Types are returned unchanged when the arities disagree.
fn substitute(ty: &StaticType, params: &[String], args: &[StaticType]) -> StaticType {
    if params.is_empty() || params.len() != args.len() {
        return ty.clone();
    }
    if ty.args.is_empty() {
        if let Some(i) = params.iter().position(|p| *p == ty.head) {
            return args[i].clone();
        }
        return ty.clone();
    }
    StaticType {
        head: ty.head.clone(),
        args: ty
            .args
            .iter()
            .map(|a| substitute(a, params, args))
            .collect(),
    }
}

/// The binding environment of one scope.
#[derive(Clone, Default)]
struct Env {
    /// Annotation-backed types; stable for the scope.
    declared: HashMap<String, StaticType>,
    /// The subset of `declared` inherited from the enclosing scope. A
    /// binding introduced in this scope replaces the inherited entry.
    inherited: HashSet<String>,
    /// Flow-sensitive types inferred from unannotated bindings.
    refined: HashMap<String, StaticType>,
    /// Names bound in this scope (or an enclosing one) with a type the
    /// pass cannot see. They mask same-named module-level declarations.
    opaque: HashSet<String>,
    /// Nested `def`s of this scope: name → declared return type.
    fns: HashMap<String, StaticType>,
    /// Nested `async def`s of this scope: name → declared return type.
    async_fns: HashMap<String, StaticType>,
    /// The class `self` belongs to, inside a method (an `impl Post:`
    /// pseudo-class counts as `Post`).
    self_class: Option<String>,
    /// `true` for the module scope, whose `def` / `class` / import
    /// bindings are described by the module facts instead.
    at_module_scope: bool,
}

impl Env {
    fn module(module: &ModModule) -> Env {
        let mut env = Env {
            at_module_scope: true,
            ..Env::default()
        };
        for stmt in &module.body {
            if let Stmt::AnnAssign(a) = stmt {
                if let (Expr::Name(n), Some(ty)) = (
                    a.target.as_ref(),
                    StaticType::from_annotation(&a.annotation),
                ) {
                    env.declared.insert(n.id.as_str().to_owned(), ty);
                }
            }
        }
        env
    }

    /// The environment a nested scope (function body, class body,
    /// lambda) starts with: the enclosing declared names and nested
    /// functions are visible, refinements are not (they describe the
    /// enclosing scope at definition time, not at call time) but their
    /// names still mask module-level declarations.
    fn nested(outer: &Env) -> Env {
        let mut opaque = outer.opaque.clone();
        opaque.extend(outer.refined.keys().cloned());
        Env {
            declared: outer.declared.clone(),
            inherited: outer.declared.keys().cloned().collect(),
            refined: HashMap::new(),
            opaque,
            fns: outer.fns.clone(),
            async_fns: outer.async_fns.clone(),
            self_class: outer.self_class.clone(),
            at_module_scope: false,
        }
    }

    fn type_of(&self, name: &str) -> Option<&StaticType> {
        self.refined.get(name).or_else(|| self.declared.get(name))
    }

    /// `true` when `name` is bound by a parameter, a local binding, or an
    /// enclosing scope's binding — anything that masks a module-level
    /// function, class, or imported name of the same spelling.
    fn is_bound(&self, name: &str) -> bool {
        self.refined.contains_key(name)
            || self.declared.contains_key(name)
            || self.opaque.contains(name)
            || self.fns.contains_key(name)
            || self.async_fns.contains_key(name)
    }

    fn drop_inherited(&mut self, name: &str) {
        if self.inherited.remove(name) {
            self.declared.remove(name);
        }
    }

    /// An annotated binding (`x: T = …`, a parameter).
    fn bind_declared(&mut self, name: &str, ty: Option<StaticType>) {
        self.drop_inherited(name);
        self.refined.remove(name);
        self.fns.remove(name);
        self.async_fns.remove(name);
        match ty {
            Some(ty) => {
                self.opaque.remove(name);
                self.declared.insert(name.to_owned(), ty);
            }
            None => {
                self.declared.remove(name);
                self.opaque.insert(name.to_owned());
            }
        }
    }

    /// An unannotated binding whose value has type `ty` when known. A
    /// declared type for the same name is kept (the checker guarantees
    /// the assignment conforms); an unknown value only clears any
    /// earlier refinement.
    fn bind_inferred(&mut self, name: &str, ty: Option<StaticType>) {
        self.drop_inherited(name);
        self.fns.remove(name);
        self.async_fns.remove(name);
        match ty {
            Some(ty) if ty.is_known() => {
                self.opaque.remove(name);
                self.refined.insert(name.to_owned(), ty);
            }
            _ => {
                self.refined.remove(name);
                if !self.declared.contains_key(name) {
                    self.opaque.insert(name.to_owned());
                }
            }
        }
    }

    fn bind_unknown(&mut self, name: &str) {
        self.bind_inferred(name, None);
    }

    /// A nested `def` / `async def`.
    fn bind_fn(&mut self, name: &str, is_async: bool, ret: Option<StaticType>) {
        self.drop_inherited(name);
        self.declared.remove(name);
        self.refined.remove(name);
        self.fns.remove(name);
        self.async_fns.remove(name);
        match ret {
            Some(ret) => {
                self.opaque.remove(name);
                if is_async {
                    self.async_fns.insert(name.to_owned(), ret);
                } else {
                    self.fns.insert(name.to_owned(), ret);
                }
            }
            None => {
                self.opaque.insert(name.to_owned());
            }
        }
    }

    /// Bind an assignment target (name, tuple/list of targets, starred)
    /// to the type of the value being unpacked into it.
    fn bind_target(&mut self, target: &Expr, ty: Option<StaticType>) {
        match target {
            Expr::Name(n) => self.bind_inferred(n.id.as_str(), ty),
            Expr::Tuple(t) => self.bind_unpack(&t.elts, ty),
            Expr::List(l) => self.bind_unpack(&l.elts, ty),
            Expr::Starred(s) => self.bind_target(&s.value, None),
            _ => {}
        }
    }

    fn bind_unpack(&mut self, elts: &[Expr], ty: Option<StaticType>) {
        let parts = ty.and_then(|t| unpack_types(&t, elts.len()));
        for (i, elt) in elts.iter().enumerate() {
            let part = parts.as_ref().map(|p| p[i].clone());
            self.bind_target(elt, part);
        }
    }
}

/// The element types an `n`-way unpack of a value of type `ty` yields.
fn unpack_types(ty: &StaticType, n: usize) -> Option<Vec<StaticType>> {
    match ty.head.as_str() {
        "tuple" => {
            if is_variadic_tuple(ty) {
                return Some(vec![ty.args[0].clone(); n]);
            }
            (ty.args.len() == n).then(|| ty.args.clone())
        }
        "list" | "set" | "frozenset" | "Sequence" | "Iterable" => {
            ty.args.first().map(|t| vec![t.clone(); n])
        }
        "str" => Some(vec![StaticType::simple("str"); n]),
        _ => None,
    }
}

fn is_variadic_tuple(ty: &StaticType) -> bool {
    ty.head == "tuple" && ty.args.len() == 2 && ty.args[1].head == "..."
}

/// Every name a statement list binds (assignments, augmented
/// assignments, loop / with / except / match targets, walrus targets,
/// `del`, imports, nested `def` / `class` names), without descending into
/// nested function or class bodies. Comprehension targets are scoped to
/// the comprehension and do not count.
fn assigned_names(body: &[Stmt]) -> HashSet<String> {
    struct Collect(HashSet<String>);
    impl<'a> Visitor<'a> for Collect {
        fn visit_stmt(&mut self, stmt: &'a Stmt) {
            match stmt {
                Stmt::FunctionDef(f) => {
                    self.0.insert(f.name.as_str().to_owned());
                    return;
                }
                Stmt::ClassDef(c) => {
                    self.0.insert(c.name.as_str().to_owned());
                    return;
                }
                Stmt::Assign(a) => {
                    for t in &a.targets {
                        collect_target_names(t, &mut self.0);
                    }
                }
                Stmt::AnnAssign(a) => collect_target_names(&a.target, &mut self.0),
                Stmt::AugAssign(a) => collect_target_names(&a.target, &mut self.0),
                Stmt::For(f) => collect_target_names(&f.target, &mut self.0),
                Stmt::With(w) => {
                    for item in &w.items {
                        if let Some(v) = &item.optional_vars {
                            collect_target_names(v, &mut self.0);
                        }
                    }
                }
                Stmt::Delete(d) => {
                    for t in &d.targets {
                        collect_target_names(t, &mut self.0);
                    }
                }
                Stmt::Try(t) => {
                    for handler in &t.handlers {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(h) = handler;
                        if let Some(n) = &h.name {
                            self.0.insert(n.as_str().to_owned());
                        }
                    }
                }
                Stmt::Import(i) => {
                    for alias in &i.names {
                        self.0.insert(import_binding(alias));
                    }
                }
                Stmt::ImportFrom(i) => {
                    for alias in &i.names {
                        self.0.insert(import_binding(alias));
                    }
                }
                Stmt::Global(g) => {
                    for n in &g.names {
                        self.0.insert(n.as_str().to_owned());
                    }
                }
                Stmt::Nonlocal(g) => {
                    for n in &g.names {
                        self.0.insert(n.as_str().to_owned());
                    }
                }
                _ => {}
            }
            visitor::walk_stmt(self, stmt);
        }
        fn visit_expr(&mut self, expr: &'a Expr) {
            if let Expr::Named(n) = expr {
                collect_target_names(&n.target, &mut self.0);
            }
            visitor::walk_expr(self, expr);
        }
        fn visit_comprehension(&mut self, comprehension: &'a Comprehension) {
            self.visit_expr(&comprehension.iter);
            for cond in &comprehension.ifs {
                self.visit_expr(cond);
            }
        }
        fn visit_pattern(&mut self, pattern: &'a Pattern) {
            collect_pattern_captures(pattern, &mut self.0);
        }
    }
    let mut collect = Collect(HashSet::new());
    for stmt in body {
        collect.visit_stmt(stmt);
    }
    collect.0
}

/// The local name an import alias binds: `import a.b` binds `a`,
/// `import a.b as c` binds `c`, `from m import x as y` binds `y`.
fn import_binding(alias: &ruff_python_ast::Alias) -> String {
    match &alias.asname {
        Some(as_name) => as_name.as_str().to_owned(),
        None => alias
            .name
            .as_str()
            .split('.')
            .next()
            .unwrap_or("")
            .to_owned(),
    }
}

fn collect_target_names(target: &Expr, out: &mut HashSet<String>) {
    match target {
        Expr::Name(n) => {
            out.insert(n.id.as_str().to_owned());
        }
        Expr::Tuple(t) => {
            for e in &t.elts {
                collect_target_names(e, out);
            }
        }
        Expr::List(l) => {
            for e in &l.elts {
                collect_target_names(e, out);
            }
        }
        Expr::Starred(s) => collect_target_names(&s.value, out),
        _ => {}
    }
}

/// Every capture name a `match` pattern binds (`case Post(title=t)`,
/// `case [x, *rest]`, `case {**extra}`, `case … as name`).
fn collect_pattern_captures(pattern: &Pattern, out: &mut HashSet<String>) {
    match pattern {
        Pattern::MatchAs(p) => {
            if let Some(n) = &p.name {
                out.insert(n.as_str().to_owned());
            }
            if let Some(inner) = &p.pattern {
                collect_pattern_captures(inner, out);
            }
        }
        Pattern::MatchStar(p) => {
            if let Some(n) = &p.name {
                out.insert(n.as_str().to_owned());
            }
        }
        Pattern::MatchMapping(p) => {
            if let Some(rest) = &p.rest {
                out.insert(rest.as_str().to_owned());
            }
            for inner in &p.patterns {
                collect_pattern_captures(inner, out);
            }
        }
        Pattern::MatchClass(p) => {
            for inner in &p.arguments.patterns {
                collect_pattern_captures(inner, out);
            }
            for kw in &p.arguments.keywords {
                collect_pattern_captures(&kw.pattern, out);
            }
        }
        Pattern::MatchSequence(p) => {
            for inner in &p.patterns {
                collect_pattern_captures(inner, out);
            }
        }
        Pattern::MatchOr(p) => {
            for inner in &p.patterns {
                collect_pattern_captures(inner, out);
            }
        }
        Pattern::MatchValue(_) | Pattern::MatchSingleton(_) => {}
    }
}

// ── receiver typing ─────────────────────────────────────────────────────────

impl RewriteCtx<'_> {
    /// The static type of a receiver expression, when it is evident.
    fn receiver_type(&self, expr: &Expr, env: &Env) -> Option<StaticType> {
        match expr {
            Expr::Name(n) => {
                let name = n.id.as_str();
                if let Some(ty) = env.type_of(name) {
                    return Some(ty.clone());
                }
                if name == "self" {
                    return env.self_class.clone().map(StaticType::simple);
                }
                None
            }
            Expr::StringLiteral(_) | Expr::FString(_) | Expr::TString(_) => {
                Some(StaticType::simple("str"))
            }
            Expr::BytesLiteral(_) => Some(StaticType::simple("bytes")),
            Expr::NumberLiteral(n) => Some(StaticType::simple(match n.value {
                ruff_python_ast::Number::Int(_) => "int",
                ruff_python_ast::Number::Float(_) => "float",
                ruff_python_ast::Number::Complex { .. } => "complex",
            })),
            Expr::BooleanLiteral(_) => Some(StaticType::simple("bool")),
            Expr::List(l) => Some(StaticType::with_args(
                "list",
                self.uniform_element(&l.elts, env),
            )),
            Expr::Set(s) => Some(StaticType::with_args(
                "set",
                self.uniform_element(&s.elts, env),
            )),
            Expr::Tuple(t) => {
                let mut args = Vec::with_capacity(t.elts.len());
                for e in &t.elts {
                    if matches!(e, Expr::Starred(_)) {
                        return Some(StaticType::simple("tuple"));
                    }
                    args.push(
                        self.receiver_type(e, env)
                            .unwrap_or_else(StaticType::unknown),
                    );
                }
                Some(StaticType::with_args("tuple", args))
            }
            Expr::Dict(d) => {
                let mut keys: Vec<&Expr> = Vec::with_capacity(d.items.len());
                let mut values: Vec<&Expr> = Vec::with_capacity(d.items.len());
                for item in &d.items {
                    let Some(key) = &item.key else {
                        // A `**splat` entry: the shape is unknowable.
                        return Some(StaticType::simple("dict"));
                    };
                    keys.push(key);
                    values.push(&item.value);
                }
                let (k, v) = (
                    self.uniform_element_refs(&keys, env),
                    self.uniform_element_refs(&values, env),
                );
                Some(match (k, v) {
                    (Some(k), Some(v)) => StaticType::with_args("dict", vec![k, v]),
                    _ => StaticType::simple("dict"),
                })
            }
            Expr::ListComp(c) => {
                let cenv = self.comprehension_env(env, &c.generators);
                Some(StaticType::with_args(
                    "list",
                    self.receiver_type(&c.elt, &cenv).into_iter().collect(),
                ))
            }
            Expr::SetComp(c) => {
                let cenv = self.comprehension_env(env, &c.generators);
                Some(StaticType::with_args(
                    "set",
                    self.receiver_type(&c.elt, &cenv).into_iter().collect(),
                ))
            }
            Expr::DictComp(c) => {
                let cenv = self.comprehension_env(env, &c.generators);
                Some(
                    match (
                        c.key.as_deref().and_then(|k| self.receiver_type(k, &cenv)),
                        self.receiver_type(&c.value, &cenv),
                    ) {
                        (Some(k), Some(v)) => StaticType::with_args("dict", vec![k, v]),
                        _ => StaticType::simple("dict"),
                    },
                )
            }
            Expr::Attribute(a) => {
                let owner = self.receiver_type(&a.value, env)?;
                self.class_member(&owner, a.attr.as_str(), Member::Field)
            }
            Expr::Call(call) => self.call_type(call, env),
            Expr::Await(a) => self.await_type(&a.value, env),
            Expr::Subscript(s) => self.subscript_type(s, env),
            Expr::BinOp(b) => {
                let l = self.receiver_type(&b.left, env)?;
                let r = self.receiver_type(&b.right, env)?;
                binop_result(b.op, &l, &r)
            }
            Expr::UnaryOp(u) => {
                let t = self.receiver_type(&u.operand, env)?;
                match u.op {
                    UnaryOp::Not => Some(StaticType::simple("bool")),
                    UnaryOp::Invert => {
                        (t.head == "int" || t.head == "bool").then(|| StaticType::simple("int"))
                    }
                    UnaryOp::UAdd | UnaryOp::USub => match t.head.as_str() {
                        "int" | "bool" => Some(StaticType::simple("int")),
                        "float" | "complex" => Some(t),
                        _ => None,
                    },
                }
            }
            Expr::If(i) => {
                let a = self.receiver_type(&i.body, env)?;
                let b = self.receiver_type(&i.orelse, env)?;
                (a == b).then_some(a)
            }
            Expr::Named(n) => self.receiver_type(&n.value, env),
            Expr::BoolOp(b) => {
                let mut first: Option<StaticType> = None;
                for v in &b.values {
                    let t = self.receiver_type(v, env)?;
                    match &first {
                        None => first = Some(t),
                        Some(f) if *f == t => {}
                        Some(_) => return None,
                    }
                }
                first
            }
            Expr::Compare(_) => Some(StaticType::simple("bool")),
            _ => None,
        }
    }

    /// The single element type shared by every element of a display, as
    /// a one-element argument list (empty when the elements disagree,
    /// are unknown, or the display is empty).
    fn uniform_element(&self, elts: &[Expr], env: &Env) -> Vec<StaticType> {
        let refs: Vec<&Expr> = elts.iter().collect();
        self.uniform_element_refs(&refs, env).into_iter().collect()
    }

    fn uniform_element_refs(&self, elts: &[&Expr], env: &Env) -> Option<StaticType> {
        let mut first: Option<StaticType> = None;
        for e in elts {
            if matches!(e, Expr::Starred(_)) {
                return None;
            }
            let t = self.receiver_type(e, env)?;
            match &first {
                None => first = Some(t),
                Some(f) if *f == t => {}
                Some(_) => return None,
            }
        }
        first
    }

    fn call_type(&self, call: &ExprCall, env: &Env) -> Option<StaticType> {
        match call.func.as_ref() {
            Expr::Name(f) => {
                let name = f.id.as_str();
                if let Some(ret) = env.fns.get(name) {
                    return Some(ret.clone());
                }
                if env.async_fns.contains_key(name) {
                    return None;
                }
                if env.is_bound(name) {
                    // A callable value: only a `Callable[[...], R]`
                    // annotation says what it returns.
                    return env.type_of(name).and_then(callable_return);
                }
                if let Some(ty) = self.builtin_call_type(name, call, env) {
                    return Some(ty);
                }
                if let Some(ret) = self.function(name) {
                    return Some(ret.clone());
                }
                if self.async_function(name).is_some() {
                    return None;
                }
                if self.is_class(name) {
                    return Some(StaticType::simple(name));
                }
                None
            }
            Expr::Attribute(a) => {
                let method = a.attr.as_str();
                if let Some(dotted) = dotted_name(&a.value) {
                    let root = dotted.split('.').next().unwrap_or(&dotted);
                    if !env.is_bound(root) && root != "self" {
                        // `alias.f()` / `alias.Cls()` on an imported module.
                        if let Some(m) = self.module(&dotted) {
                            if let Some(ret) = m.functions.get(method) {
                                return Some(ret.clone());
                            }
                            if m.classes.contains_key(method) {
                                return Some(StaticType::simple(format!("{dotted}.{method}")));
                            }
                            return None;
                        }
                        // `Cls.static_method()` on a known class.
                        if self.is_class(&dotted) {
                            return self.class_member(
                                &StaticType::simple(&dotted),
                                method,
                                Member::StaticMethod,
                            );
                        }
                    }
                }
                let owner = self.receiver_type(&a.value, env)?;
                // An extension method (rewritten or not): the lifted free
                // function's declared return.
                if let Some(fn_name) = self.extension_fn(&owner, method) {
                    return self.function(fn_name).cloned();
                }
                if let Some(ty) = builtin_method_type(&owner, method) {
                    return Some(ty);
                }
                self.class_member(&owner, method, Member::Method)
            }
            _ => None,
        }
    }

    /// Calls to builtin constructors and functions with a fixed result
    /// type (`str(x)`, `len(x)`, `sorted(xs)`, …).
    fn builtin_call_type(&self, name: &str, call: &ExprCall, env: &Env) -> Option<StaticType> {
        let first_arg = call.arguments.args.first();
        let elem_of_first = || {
            first_arg
                .and_then(|a| self.element_type(a, env))
                .into_iter()
                .collect::<Vec<_>>()
        };
        Some(match name {
            "str" | "repr" | "format" | "chr" | "ascii" | "hex" | "oct" | "bin" | "input" => {
                StaticType::simple("str")
            }
            "bytes" => StaticType::simple("bytes"),
            "bytearray" => StaticType::simple("bytearray"),
            "int" | "len" | "ord" | "hash" | "id" => StaticType::simple("int"),
            "float" => StaticType::simple("float"),
            "bool" => StaticType::simple("bool"),
            "abs" => match first_arg.and_then(|a| self.receiver_type(a, env)) {
                Some(t) if matches!(t.head.as_str(), "int" | "float") => t,
                Some(t) if t.head == "bool" => StaticType::simple("int"),
                _ => return None,
            },
            // `round(x)` is an `int`; `round(x, n)` keeps `x`'s type.
            "round" => match call.arguments.args.len() {
                1 => StaticType::simple("int"),
                2 => match first_arg.and_then(|a| self.receiver_type(a, env)) {
                    Some(t) if matches!(t.head.as_str(), "int" | "float") => t,
                    _ => return None,
                },
                _ => return None,
            },
            // `sum` of an `int` iterable is an `int`, of a `float` one a
            // `float`; a `start` argument may change that, so only the
            // one-argument form is typed.
            "sum" if call.arguments.args.len() == 1 => match elem_of_first().pop() {
                Some(t) if matches!(t.head.as_str(), "int" | "float") => t,
                Some(t) if t.head == "bool" => StaticType::simple("int"),
                _ => return None,
            },
            // `min(xs)` / `max(xs)` yield an element; `min(a, b, …)` the
            // arguments' shared type.
            "min" | "max" => {
                let args = &call.arguments.args;
                let ty = if args.len() == 1 {
                    elem_of_first().pop()
                } else {
                    self.uniform_element(args, env).pop()
                };
                ty?
            }
            "list" | "sorted" => StaticType::with_args("list", elem_of_first()),
            "set" => StaticType::with_args("set", elem_of_first()),
            "frozenset" => StaticType::with_args("frozenset", elem_of_first()),
            "tuple" => match elem_of_first().pop() {
                Some(t) => StaticType::with_args("tuple", vec![t, StaticType::simple("...")]),
                None => StaticType::simple("tuple"),
            },
            "dict" => match first_arg.and_then(|a| self.receiver_type(a, env)) {
                Some(t) if t.head == "dict" => t,
                _ => StaticType::simple("dict"),
            },
            "range" => StaticType::simple("range"),
            _ => return None,
        })
    }

    fn await_type(&self, inner: &Expr, env: &Env) -> Option<StaticType> {
        let Expr::Call(call) = inner else {
            return self
                .receiver_type(inner, env)
                .and_then(|t| awaited_value(&t));
        };
        match call.func.as_ref() {
            Expr::Name(f) => {
                let name = f.id.as_str();
                if let Some(ret) = env.async_fns.get(name) {
                    return Some(ret.clone());
                }
                if let Some(ret) = env.fns.get(name) {
                    return awaited_value(ret);
                }
                if env.is_bound(name) {
                    return env
                        .type_of(name)
                        .and_then(callable_return)
                        .and_then(|t| awaited_value(&t));
                }
                if let Some(ret) = self.async_function(name) {
                    return Some(ret.clone());
                }
                self.function(name).and_then(awaited_value)
            }
            Expr::Attribute(a) => {
                let method = a.attr.as_str();
                if let Some(dotted) = dotted_name(&a.value) {
                    let root = dotted.split('.').next().unwrap_or(&dotted);
                    if !env.is_bound(root) && root != "self" {
                        if let Some(m) = self.module(&dotted) {
                            if let Some(ret) = m.async_functions.get(method) {
                                return Some(ret.clone());
                            }
                            return m.functions.get(method).and_then(awaited_value);
                        }
                    }
                }
                let owner = self.receiver_type(&a.value, env)?;
                if let Some(ret) = self.class_member(&owner, method, Member::AsyncMethod) {
                    return Some(ret);
                }
                self.class_member(&owner, method, Member::Method)
                    .and_then(|t| awaited_value(&t))
            }
            _ => None,
        }
    }

    fn subscript_type(&self, s: &ruff_python_ast::ExprSubscript, env: &Env) -> Option<StaticType> {
        let base = self.receiver_type(&s.value, env)?;
        if matches!(s.slice.as_ref(), Expr::Slice(_)) {
            return match base.head.as_str() {
                "list" | "str" | "bytes" | "bytearray" => Some(base),
                "tuple" if is_variadic_tuple(&base) => Some(base),
                "tuple" => Some(StaticType::simple("tuple")),
                _ => None,
            };
        }
        match base.head.as_str() {
            "list" | "Sequence" | "MutableSequence" | "deque" => base.args.first().cloned(),
            "str" => Some(StaticType::simple("str")),
            "bytes" | "bytearray" => Some(StaticType::simple("int")),
            "dict" | "Mapping" | "MutableMapping" | "defaultdict" | "OrderedDict" => {
                base.args.get(1).cloned()
            }
            "tuple" => {
                if base.args.is_empty() {
                    return None;
                }
                if is_variadic_tuple(&base) {
                    return Some(base.args[0].clone());
                }
                if let Some(i) = int_literal(&s.slice) {
                    let n = base.args.len() as i64;
                    let idx = if i < 0 { i + n } else { i };
                    if (0..n).contains(&idx) {
                        return Some(base.args[idx as usize].clone());
                    }
                    return None;
                }
                base.args
                    .iter()
                    .all(|a| *a == base.args[0])
                    .then(|| base.args[0].clone())
            }
            _ => None,
        }
        .filter(StaticType::is_known)
    }

    /// The type of one element produced by iterating `iter`.
    fn element_type(&self, iter: &Expr, env: &Env) -> Option<StaticType> {
        if let Expr::Call(call) = iter {
            match call.func.as_ref() {
                Expr::Name(f) if !env.is_bound(f.id.as_str()) => {
                    let args = &call.arguments.args;
                    match f.id.as_str() {
                        "range" => return Some(StaticType::simple("int")),
                        "enumerate" => {
                            let inner = args
                                .first()
                                .and_then(|a| self.element_type(a, env))
                                .unwrap_or_else(StaticType::unknown);
                            return Some(StaticType::with_args(
                                "tuple",
                                vec![StaticType::simple("int"), inner],
                            ));
                        }
                        "zip" => {
                            let parts: Vec<StaticType> = args
                                .iter()
                                .map(|a| {
                                    self.element_type(a, env)
                                        .unwrap_or_else(StaticType::unknown)
                                })
                                .collect();
                            return Some(StaticType::with_args("tuple", parts));
                        }
                        "reversed" | "sorted" | "iter" | "list" | "tuple" | "set" | "frozenset" => {
                            return args.first().and_then(|a| self.element_type(a, env));
                        }
                        _ => {}
                    }
                }
                Expr::Attribute(a) => {
                    let method = a.attr.as_str();
                    if matches!(method, "items" | "keys" | "values") {
                        if let Some(owner) = self.receiver_type(&a.value, env) {
                            if owner.head == "dict" && owner.args.len() == 2 {
                                return Some(match method {
                                    "items" => StaticType::with_args("tuple", owner.args.clone()),
                                    "keys" => owner.args[0].clone(),
                                    _ => owner.args[1].clone(),
                                });
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let ty = self.receiver_type(iter, env)?;
        match ty.head.as_str() {
            "list" | "set" | "frozenset" | "Sequence" | "MutableSequence" | "Iterable"
            | "Iterator" | "Collection" | "AbstractSet" | "MutableSet" | "deque" | "Generator" => {
                ty.args.first().cloned()
            }
            "tuple" => {
                if ty.args.is_empty() {
                    return None;
                }
                if is_variadic_tuple(&ty) {
                    return Some(ty.args[0].clone());
                }
                ty.args
                    .iter()
                    .all(|a| *a == ty.args[0])
                    .then(|| ty.args[0].clone())
            }
            "dict" | "Mapping" | "MutableMapping" | "defaultdict" | "OrderedDict" => {
                ty.args.first().cloned()
            }
            "str" => Some(StaticType::simple("str")),
            "bytes" | "bytearray" | "range" => Some(StaticType::simple("int")),
            _ => None,
        }
        .filter(StaticType::is_known)
    }

    /// The environment inside a comprehension: the enclosing one plus
    /// each generator target bound to its iterable's element type.
    fn comprehension_env(&self, env: &Env, generators: &[Comprehension]) -> Env {
        let mut cenv = env.clone();
        for generator in generators {
            let elem = self.element_type(&generator.iter, &cenv);
            cenv.bind_target(&generator.target, elem);
        }
        cenv
    }
}

/// The return type of a `Callable[[...], R]` annotation.
fn callable_return(ty: &StaticType) -> Option<StaticType> {
    (ty.head.rsplit('.').next() == Some("Callable") && ty.args.len() == 2)
        .then(|| ty.args[1].clone())
        .filter(StaticType::is_known)
}

/// The value an `await` of a value of type `ty` produces.
fn awaited_value(ty: &StaticType) -> Option<StaticType> {
    match ty.head.rsplit('.').next().unwrap_or(&ty.head) {
        "Awaitable" | "Task" | "Future" => ty.args.first().cloned(),
        "Coroutine" => ty.args.get(2).cloned(),
        _ => None,
    }
    .filter(StaticType::is_known)
}

/// A literal integer index, including a negated one.
fn int_literal(expr: &Expr) -> Option<i64> {
    match expr {
        Expr::NumberLiteral(n) => match &n.value {
            ruff_python_ast::Number::Int(i) => i.as_i64(),
            _ => None,
        },
        Expr::UnaryOp(u) if matches!(u.op, UnaryOp::USub) => int_literal(&u.operand).map(|i| -i),
        _ => None,
    }
}

/// The result type of a binary operation between two builtin operands,
/// when Python's semantics fix it.
fn binop_result(op: Operator, l: &StaticType, r: &StaticType) -> Option<StaticType> {
    let (lh, rh) = (l.head.as_str(), r.head.as_str());
    let numeric = || match (lh, rh) {
        ("int" | "bool", "int" | "bool") => Some(StaticType::simple("int")),
        ("float", "int" | "bool" | "float") | ("int" | "bool", "float") => {
            Some(StaticType::simple("float"))
        }
        _ => None,
    };
    match op {
        Operator::Add => match (lh, rh) {
            ("str", "str") => Some(StaticType::simple("str")),
            ("bytes", "bytes") => Some(StaticType::simple("bytes")),
            ("list", "list") => Some(if l == r {
                l.clone()
            } else {
                StaticType::simple("list")
            }),
            ("tuple", "tuple") => Some(StaticType::simple("tuple")),
            _ => numeric(),
        },
        Operator::Mult => match (lh, rh) {
            ("str", "int" | "bool") | ("int" | "bool", "str") => Some(StaticType::simple("str")),
            ("bytes", "int" | "bool") | ("int" | "bool", "bytes") => {
                Some(StaticType::simple("bytes"))
            }
            ("list", "int" | "bool") => Some(l.clone()),
            ("int" | "bool", "list") => Some(r.clone()),
            ("tuple", "int" | "bool") | ("int" | "bool", "tuple") => {
                Some(StaticType::simple("tuple"))
            }
            _ => numeric(),
        },
        Operator::Mod => match lh {
            "str" => Some(StaticType::simple("str")),
            "bytes" => Some(StaticType::simple("bytes")),
            _ => numeric(),
        },
        Operator::Sub => match (lh, rh) {
            ("set", "set") | ("frozenset", "frozenset") => Some(l.clone()),
            _ => numeric(),
        },
        Operator::FloorDiv | Operator::Pow => numeric(),
        Operator::Div => numeric().map(|_| StaticType::simple("float")),
        Operator::BitOr | Operator::BitAnd | Operator::BitXor => match (lh, rh) {
            ("set", "set") | ("frozenset", "frozenset") => Some(l.clone()),
            ("dict", "dict") if matches!(op, Operator::BitOr) => Some(if l == r {
                l.clone()
            } else {
                StaticType::simple("dict")
            }),
            ("int" | "bool", "int" | "bool") => Some(StaticType::simple("int")),
            _ => None,
        },
        _ => None,
    }
}

/// The result type of a builtin method whose receiver type is known:
/// the type-preserving methods (`t.strip().lower()`), plus a small table
/// of element- and container-producing ones.
fn builtin_method_type(owner: &StaticType, method: &str) -> Option<StaticType> {
    let same = || Some(owner.clone());
    let simple = |h: &str| Some(StaticType::simple(h));
    match owner.head.as_str() {
        "str" => match method {
            "strip" | "lstrip" | "rstrip" | "lower" | "upper" | "title" | "capitalize"
            | "casefold" | "swapcase" | "replace" | "join" | "format" | "format_map" | "center"
            | "ljust" | "rjust" | "zfill" | "removeprefix" | "removesuffix" | "expandtabs"
            | "translate" => simple("str"),
            "split" | "rsplit" | "splitlines" => Some(StaticType::with_args(
                "list",
                vec![StaticType::simple("str")],
            )),
            "partition" | "rpartition" => Some(StaticType::with_args(
                "tuple",
                vec![StaticType::simple("str"); 3],
            )),
            "encode" => simple("bytes"),
            "find" | "rfind" | "index" | "rindex" | "count" => simple("int"),
            "startswith" | "endswith" | "isdigit" | "isalpha" | "isalnum" | "isspace"
            | "isupper" | "islower" | "istitle" | "isnumeric" | "isdecimal" | "isidentifier"
            | "isprintable" | "isascii" => simple("bool"),
            _ => None,
        },
        "bytes" => match method {
            "strip" | "lstrip" | "rstrip" | "lower" | "upper" | "replace" | "join" => {
                simple("bytes")
            }
            "decode" | "hex" => simple("str"),
            "split" | "rsplit" | "splitlines" => Some(StaticType::with_args(
                "list",
                vec![StaticType::simple("bytes")],
            )),
            "find" | "rfind" | "index" | "rindex" | "count" => simple("int"),
            _ => None,
        },
        "list" => match method {
            "copy" => same(),
            "pop" => owner.args.first().cloned(),
            "index" | "count" => simple("int"),
            _ => None,
        },
        "dict" => match method {
            "copy" => same(),
            // `get` yields `V?`, which reads as `V` like every other
            // nullable receiver (the checker requires the narrowing).
            "pop" | "get" | "setdefault" => owner.args.get(1).cloned(),
            _ => None,
        },
        "set" | "frozenset" => match method {
            "copy" | "union" | "intersection" | "difference" | "symmetric_difference" => same(),
            "pop" => owner.args.first().cloned(),
            _ => None,
        },
        "tuple" => match method {
            "count" | "index" => simple("int"),
            _ => None,
        },
        "int" => match method {
            "bit_length" | "bit_count" => simple("int"),
            "to_bytes" => simple("bytes"),
            _ => None,
        },
        "float" => match method {
            "is_integer" => simple("bool"),
            "hex" => simple("str"),
            _ => None,
        },
        _ => None,
    }
}

// ── the AST walk ────────────────────────────────────────────────────────────

struct Walker<'a> {
    ctx: &'a RewriteCtx<'a>,
    rewrites: usize,
    used_fns: HashSet<String>,
}

impl Walker<'_> {
    fn body(&mut self, body: Vec<Stmt>, env: &mut Env) -> Vec<Stmt> {
        body.into_iter().map(|s| self.stmt(s, env)).collect()
    }

    /// Walk a nested block. Refinements made inside it do not survive it,
    /// and a name the block assigns loses its pre-block refinement (the
    /// block may or may not have run).
    fn block(&mut self, body: Vec<Stmt>, env: &mut Env) -> Vec<Stmt> {
        let assigned = assigned_names(&body);
        let saved = env.refined.clone();
        let out = self.body(body, env);
        env.refined = saved;
        for name in &assigned {
            env.refined.remove(name);
        }
        out
    }

    /// Walk one statement, rewriting eligible attribute calls in any
    /// expression position and recursing into every nested
    /// statement-bearing node (`for`, `while`, `with`, `try`, `match`, …).
    ///
    /// `env` is the current scope's binding environment. This pass
    /// mutates it as it sees bindings so a subsequent statement in the
    /// same block can rely on them. Function and class bodies open a
    /// nested scope (see [`Env::nested`]).
    fn stmt(&mut self, stmt: Stmt, env: &mut Env) -> Stmt {
        match stmt {
            Stmt::FunctionDef(mut f) => {
                // Decorators and parameter defaults evaluate in the
                // enclosing scope at definition time.
                for decorator in &mut f.decorator_list {
                    self.expr(&mut decorator.expression, env);
                }
                for param in f
                    .parameters
                    .posonlyargs
                    .iter_mut()
                    .chain(f.parameters.args.iter_mut())
                    .chain(f.parameters.kwonlyargs.iter_mut())
                {
                    if let Some(default) = param.default.as_mut() {
                        self.expr(default, env);
                    }
                }
                if !env.at_module_scope {
                    env.bind_fn(f.name.as_str(), f.is_async, declared_return(&f));
                }
                let mut local = Env::nested(env);
                for param in f
                    .parameters
                    .posonlyargs
                    .iter()
                    .chain(f.parameters.args.iter())
                    .chain(f.parameters.kwonlyargs.iter())
                {
                    local.bind_declared(
                        param.parameter.name.as_str(),
                        param
                            .parameter
                            .annotation
                            .as_deref()
                            .and_then(StaticType::from_annotation),
                    );
                }
                if let Some(vararg) = &f.parameters.vararg {
                    let elem = vararg
                        .annotation
                        .as_deref()
                        .and_then(StaticType::from_annotation);
                    local.bind_declared(
                        vararg.name.as_str(),
                        elem.map(|t| {
                            StaticType::with_args("tuple", vec![t, StaticType::simple("...")])
                        }),
                    );
                }
                if let Some(kwarg) = &f.parameters.kwarg {
                    let value = kwarg
                        .annotation
                        .as_deref()
                        .and_then(StaticType::from_annotation);
                    local.bind_declared(
                        kwarg.name.as_str(),
                        value.map(|t| {
                            StaticType::with_args("dict", vec![StaticType::simple("str"), t])
                        }),
                    );
                }
                f.body = self.body(std::mem::take(&mut f.body), &mut local);
                Stmt::FunctionDef(f)
            }
            Stmt::ClassDef(mut c) => {
                for decorator in &mut c.decorator_list {
                    self.expr(&mut decorator.expression, env);
                }
                if let Some(args) = c.arguments.as_mut() {
                    for base in &mut args.args {
                        self.expr(base, env);
                    }
                    for kw in &mut args.keywords {
                        self.expr(&mut kw.value, env);
                    }
                }
                if !env.at_module_scope {
                    env.bind_unknown(c.name.as_str());
                }
                // Class-level statements see the enclosing scope; the
                // class's own bindings are not visible inside its
                // methods (Python's class scope does not nest), so each
                // method starts from the enclosing scope plus `self`.
                let class_name = c
                    .name
                    .as_str()
                    .strip_prefix(IMPL_PREFIX)
                    .unwrap_or(c.name.as_str())
                    .to_owned();
                let mut class_env = Env::nested(env);
                let mut method_outer = env.clone();
                method_outer.self_class = Some(class_name);
                let mut new_body = Vec::with_capacity(c.body.len());
                for member in std::mem::take(&mut c.body) {
                    match member {
                        Stmt::FunctionDef(m) => {
                            let mut outer = method_outer.clone();
                            new_body.push(self.stmt(Stmt::FunctionDef(m), &mut outer));
                        }
                        other => new_body.push(self.stmt(other, &mut class_env)),
                    }
                }
                c.body = new_body;
                Stmt::ClassDef(c)
            }
            Stmt::AnnAssign(mut a) => {
                if let Some(v) = a.value.as_mut() {
                    let expected = StaticType::from_annotation(&a.annotation);
                    self.expr_expected(v, env, expected.as_ref());
                }
                if let Expr::Name(n) = a.target.as_ref() {
                    env.bind_declared(n.id.as_str(), StaticType::from_annotation(&a.annotation));
                } else {
                    self.expr(&mut a.target, env);
                }
                Stmt::AnnAssign(a)
            }
            Stmt::Assign(mut a) => {
                self.expr(&mut a.value, env);
                let ty = self.ctx.receiver_type(&a.value, env);
                for target in &mut a.targets {
                    self.expr(target, env);
                    env.bind_target(target, ty.clone());
                }
                Stmt::Assign(a)
            }
            Stmt::AugAssign(mut a) => {
                self.expr(&mut a.value, env);
                self.expr(&mut a.target, env);
                if let Expr::Name(n) = a.target.as_ref() {
                    let result = env.type_of(n.id.as_str()).cloned().and_then(|l| {
                        self.ctx
                            .receiver_type(&a.value, env)
                            .and_then(|r| binop_result(a.op, &l, &r))
                    });
                    env.bind_inferred(n.id.as_str(), result);
                }
                Stmt::AugAssign(a)
            }
            Stmt::Expr(mut e) => {
                self.expr(&mut e.value, env);
                Stmt::Expr(e)
            }
            Stmt::Return(mut r) => {
                if let Some(v) = r.value.as_mut() {
                    self.expr(v, env);
                }
                Stmt::Return(r)
            }
            Stmt::Raise(mut r) => {
                if let Some(v) = r.exc.as_mut() {
                    self.expr(v, env);
                }
                if let Some(v) = r.cause.as_mut() {
                    self.expr(v, env);
                }
                Stmt::Raise(r)
            }
            Stmt::Assert(mut a) => {
                self.expr(&mut a.test, env);
                if let Some(m) = a.msg.as_mut() {
                    self.expr(m, env);
                }
                Stmt::Assert(a)
            }
            Stmt::Delete(mut d) => {
                for t in &mut d.targets {
                    self.expr(t, env);
                    if let Expr::Name(n) = t {
                        env.bind_unknown(n.id.as_str());
                    }
                }
                Stmt::Delete(d)
            }
            Stmt::Global(g) => {
                for n in &g.names {
                    env.refined.remove(n.as_str());
                }
                Stmt::Global(g)
            }
            Stmt::Nonlocal(g) => {
                for n in &g.names {
                    env.refined.remove(n.as_str());
                }
                Stmt::Nonlocal(g)
            }
            Stmt::Import(i) => {
                if !env.at_module_scope {
                    for alias in &i.names {
                        env.bind_unknown(&import_binding(alias));
                    }
                }
                Stmt::Import(i)
            }
            Stmt::ImportFrom(i) => {
                if !env.at_module_scope {
                    for alias in &i.names {
                        env.bind_unknown(&import_binding(alias));
                    }
                }
                Stmt::ImportFrom(i)
            }
            Stmt::If(mut i) => {
                self.expr(&mut i.test, env);
                i.body = self.block(std::mem::take(&mut i.body), env);
                for clause in &mut i.elif_else_clauses {
                    if let Some(t) = clause.test.as_mut() {
                        self.expr(t, env);
                    }
                    clause.body = self.block(std::mem::take(&mut clause.body), env);
                }
                Stmt::If(i)
            }
            Stmt::While(mut w) => {
                // Every name the loop reassigns is unknown at its head.
                let mut assigned = assigned_names(&w.body);
                assigned.extend(assigned_names(&w.orelse));
                for name in &assigned {
                    env.refined.remove(name);
                }
                self.expr(&mut w.test, env);
                w.body = self.block(std::mem::take(&mut w.body), env);
                w.orelse = self.block(std::mem::take(&mut w.orelse), env);
                Stmt::While(w)
            }
            Stmt::For(mut f) => {
                self.expr(&mut f.target, env);
                self.expr(&mut f.iter, env);
                let elem = self.ctx.element_type(&f.iter, env);
                let mut assigned = assigned_names(&f.body);
                assigned.extend(assigned_names(&f.orelse));
                collect_target_names(&f.target, &mut assigned);
                for name in &assigned {
                    env.refined.remove(name);
                }
                let saved = env.refined.clone();
                env.bind_target(&f.target, elem);
                f.body = self.body(std::mem::take(&mut f.body), env);
                env.refined = saved.clone();
                f.orelse = self.body(std::mem::take(&mut f.orelse), env);
                env.refined = saved;
                Stmt::For(f)
            }
            Stmt::With(mut w) => {
                for item in &mut w.items {
                    self.expr(&mut item.context_expr, env);
                    if let Some(v) = item.optional_vars.as_mut() {
                        self.expr(v, env);
                        env.bind_target(v, None);
                    }
                }
                w.body = self.block(std::mem::take(&mut w.body), env);
                Stmt::With(w)
            }
            Stmt::Try(mut t) => {
                t.body = self.block(std::mem::take(&mut t.body), env);
                for handler in &mut t.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = handler;
                    if let Some(ty) = h.type_.as_mut() {
                        self.expr(ty, env);
                    }
                    if let Some(n) = &h.name {
                        env.bind_unknown(n.as_str());
                    }
                    h.body = self.block(std::mem::take(&mut h.body), env);
                }
                t.orelse = self.block(std::mem::take(&mut t.orelse), env);
                t.finalbody = self.block(std::mem::take(&mut t.finalbody), env);
                Stmt::Try(t)
            }
            Stmt::Match(mut m) => {
                self.expr(&mut m.subject, env);
                for case in &mut m.cases {
                    let mut captures = HashSet::new();
                    collect_pattern_captures(&case.pattern, &mut captures);
                    let mut case_env = Env::nested(env);
                    for name in &captures {
                        case_env.bind_unknown(name);
                    }
                    let subject = self.ctx.receiver_type(&m.subject, env);
                    self.bind_pattern(&case.pattern, subject.as_ref(), &mut case_env);
                    if let Some(g) = case.guard.as_mut() {
                        self.expr(g, &mut case_env);
                    }
                    case.body = self.block(std::mem::take(&mut case.body), &mut case_env);
                }
                Stmt::Match(m)
            }
            other => other,
        }
    }

    fn expr_expected(&mut self, expr: &mut Expr, env: &mut Env, expected: Option<&StaticType>) {
        if let (Expr::Lambda(l), Some(expected)) = (&mut *expr, expected) {
            if expected.head.rsplit('.').next() == Some("Callable") {
                let mut local = Env::nested(env);
                if let Some(params) = l.parameters.as_deref() {
                    let types = expected.args.first().filter(|t| t.head == "parameters");
                    for (i, param) in params
                        .posonlyargs
                        .iter()
                        .chain(params.args.iter())
                        .enumerate()
                    {
                        local.bind_declared(
                            param.parameter.name.as_str(),
                            types.and_then(|t| t.args.get(i)).cloned(),
                        );
                    }
                    for param in &params.kwonlyargs {
                        local.bind_unknown(param.parameter.name.as_str());
                    }
                    if let Some(p) = &params.vararg {
                        local.bind_unknown(p.name.as_str());
                    }
                    if let Some(p) = &params.kwarg {
                        local.bind_unknown(p.name.as_str());
                    }
                }
                self.expr_expected(&mut l.body, &mut local, expected.args.get(1));
                return;
            }
        }
        self.expr(expr, env);
    }

    fn bind_pattern(&self, pattern: &Pattern, ty: Option<&StaticType>, env: &mut Env) {
        match pattern {
            Pattern::MatchAs(p) => {
                if let Some(name) = &p.name {
                    env.bind_declared(name.as_str(), ty.cloned());
                }
                if let Some(inner) = &p.pattern {
                    self.bind_pattern(inner, ty, env);
                }
            }
            Pattern::MatchSequence(p) => {
                for (i, inner) in p.patterns.iter().enumerate() {
                    let elem = ty.and_then(|t| {
                        if t.head == "tuple" {
                            t.args.get(i)
                        } else {
                            t.args.first()
                        }
                    });
                    if let Pattern::MatchStar(star) = inner {
                        if let Some(name) = &star.name {
                            env.bind_declared(
                                name.as_str(),
                                elem.cloned()
                                    .map(|t| StaticType::with_args("list", vec![t])),
                            );
                        }
                    } else {
                        self.bind_pattern(inner, elem, env);
                    }
                }
            }
            Pattern::MatchMapping(p) => {
                for inner in &p.patterns {
                    self.bind_pattern(inner, ty.and_then(|t| t.args.get(1)), env);
                }
                if let Some(name) = &p.rest {
                    env.bind_declared(name.as_str(), ty.cloned());
                }
            }
            Pattern::MatchClass(p) => {
                let owner = dotted_name(&p.cls).map(StaticType::simple);
                for kw in &p.arguments.keywords {
                    let field = owner
                        .as_ref()
                        .and_then(|t| self.ctx.class_member(t, kw.attr.as_str(), Member::Field));
                    self.bind_pattern(&kw.pattern, field.as_ref(), env);
                }
            }
            Pattern::MatchOr(p) => {
                for inner in &p.patterns {
                    self.bind_pattern(inner, ty, env);
                }
            }
            _ => {}
        }
    }

    /// Rewrite eligible attribute calls anywhere inside `expr`. Descends
    /// through every expression variant so a call buried in
    /// `x.shout() + "!"` or `f(x.shout())` reaches the rewrite logic.
    fn expr(&mut self, expr: &mut Expr, env: &mut Env) {
        if let Expr::Call(call) = expr {
            for arg in &mut call.arguments.args {
                self.expr(arg, env);
            }
            for kw in &mut call.arguments.keywords {
                self.expr(&mut kw.value, env);
            }
            let rewrite = if let Expr::Attribute(attr) = call.func.as_mut() {
                // The receiver first, so a nested extension call
                // (`x.slug().shout()`) is lowered before the outer call
                // is typed against the lifted function's return type.
                self.expr(&mut attr.value, env);
                self.ctx
                    .receiver_type(&attr.value, env)
                    .and_then(|owner| self.ctx.extension_fn(&owner, attr.attr.as_str()))
                    .cloned()
            } else {
                // Not an attribute call — still descend into the callee
                // so a receiver buried inside a more complex call
                // expression (lambda, subscript) gets visited.
                self.expr(&mut call.func, env);
                None
            };
            if let Some(fn_name) = rewrite {
                let Expr::Attribute(attr) = call.func.as_mut() else {
                    unreachable!("rewrite is only planned for an attribute callee");
                };
                let range = call.range;
                let receiver = std::mem::replace(
                    attr.value.as_mut(),
                    Expr::Name(ExprName {
                        range,
                        node_index: AtomicNodeIndex::NONE,
                        id: Name::new(""),
                        ctx: ruff_python_ast::ExprContext::Load,
                    }),
                );
                let mut new_args: Vec<Expr> = Vec::with_capacity(call.arguments.args.len() + 1);
                new_args.push(receiver);
                new_args.extend(std::mem::take(&mut call.arguments.args).into_vec());
                let new_call = ExprCall {
                    range,
                    node_index: AtomicNodeIndex::NONE,
                    func: Box::new(Expr::Name(ExprName {
                        range,
                        node_index: AtomicNodeIndex::NONE,
                        id: Name::new(&fn_name),
                        ctx: ruff_python_ast::ExprContext::Load,
                    })),
                    arguments: ruff_python_ast::Arguments {
                        range,
                        node_index: AtomicNodeIndex::NONE,
                        args: new_args.into_boxed_slice(),
                        keywords: std::mem::take(&mut call.arguments.keywords),
                    },
                };
                *expr = Expr::Call(new_call);
                self.rewrites += 1;
                self.used_fns.insert(fn_name);
            }
            return;
        }

        // Generic recursion through every Expr variant. The goal is
        // coverage, not pretty matching — every shape that can contain
        // a sub-expression descends.
        match expr {
            Expr::BoolOp(b) => {
                for v in &mut b.values {
                    self.expr(v, env);
                }
            }
            Expr::Named(n) => {
                self.expr(&mut n.value, env);
                let ty = self.ctx.receiver_type(&n.value, env);
                env.bind_target(&n.target, ty);
            }
            Expr::BinOp(b) => {
                self.expr(&mut b.left, env);
                self.expr(&mut b.right, env);
            }
            Expr::UnaryOp(u) => self.expr(&mut u.operand, env),
            Expr::Lambda(l) => {
                // A lambda closes over the enclosing scope; its own
                // parameters are unannotated and mask anything outside.
                let mut local = Env::nested(env);
                if let Some(params) = l.parameters.as_deref() {
                    for param in params
                        .posonlyargs
                        .iter()
                        .chain(params.args.iter())
                        .chain(params.kwonlyargs.iter())
                    {
                        local.bind_declared(param.parameter.name.as_str(), None);
                    }
                    if let Some(vararg) = &params.vararg {
                        local.bind_declared(vararg.name.as_str(), None);
                    }
                    if let Some(kwarg) = &params.kwarg {
                        local.bind_declared(kwarg.name.as_str(), None);
                    }
                }
                self.expr(&mut l.body, &mut local);
            }
            Expr::If(i) => {
                self.expr(&mut i.test, env);
                self.expr(&mut i.body, env);
                self.expr(&mut i.orelse, env);
            }
            Expr::Dict(d) => {
                for item in &mut d.items {
                    if let Some(k) = item.key.as_mut() {
                        self.expr(k, env);
                    }
                    self.expr(&mut item.value, env);
                }
            }
            Expr::Set(s) => {
                for e in &mut s.elts {
                    self.expr(e, env);
                }
            }
            Expr::ListComp(c) => {
                let mut cenv = self.comprehension(&mut c.generators, env);
                self.expr(&mut c.elt, &mut cenv);
            }
            Expr::SetComp(c) => {
                let mut cenv = self.comprehension(&mut c.generators, env);
                self.expr(&mut c.elt, &mut cenv);
            }
            Expr::DictComp(c) => {
                let mut cenv = self.comprehension(&mut c.generators, env);
                if let Some(k) = c.key.as_mut() {
                    self.expr(k, &mut cenv);
                }
                self.expr(&mut c.value, &mut cenv);
            }
            Expr::Generator(g) => {
                let mut cenv = self.comprehension(&mut g.generators, env);
                self.expr(&mut g.elt, &mut cenv);
            }
            Expr::Await(a) => self.expr(&mut a.value, env),
            Expr::Yield(y) => {
                if let Some(v) = y.value.as_mut() {
                    self.expr(v, env);
                }
            }
            Expr::YieldFrom(y) => self.expr(&mut y.value, env),
            Expr::Compare(c) => {
                self.expr(&mut c.left, env);
                for cmp in &mut c.comparators {
                    self.expr(cmp, env);
                }
            }
            Expr::Attribute(a) => self.expr(&mut a.value, env),
            Expr::Subscript(s) => {
                self.expr(&mut s.value, env);
                self.expr(&mut s.slice, env);
            }
            Expr::Starred(s) => self.expr(&mut s.value, env),
            Expr::List(l) => {
                for e in &mut l.elts {
                    self.expr(e, env);
                }
            }
            Expr::Tuple(t) => {
                for e in &mut t.elts {
                    self.expr(e, env);
                }
            }
            Expr::Slice(s) => {
                if let Some(v) = s.lower.as_mut() {
                    self.expr(v, env);
                }
                if let Some(v) = s.upper.as_mut() {
                    self.expr(v, env);
                }
                if let Some(v) = s.step.as_mut() {
                    self.expr(v, env);
                }
            }
            // Leaves and the previously-handled `Call` are no-ops here.
            _ => {}
        }
    }

    /// Walk a comprehension's generators (the first iterable evaluates
    /// in the enclosing scope, the rest inside the comprehension) and
    /// return the environment its element expression sees.
    fn comprehension(&mut self, generators: &mut [Comprehension], env: &mut Env) -> Env {
        let mut cenv = Env::nested(env);
        // Inside a comprehension the enclosing scope's refinements are
        // still current — it runs right here, not later.
        cenv.refined = env.refined.clone();
        for (i, generator) in generators.iter_mut().enumerate() {
            if i == 0 {
                self.expr(&mut generator.iter, env);
            } else {
                self.expr(&mut generator.iter, &mut cenv);
            }
            let elem = self.ctx.element_type(&generator.iter, &cenv);
            cenv.bind_target(&generator.target, elem);
            for cond in &mut generator.ifs {
                self.expr(cond, &mut cenv);
            }
        }
        cenv
    }
}

/// Avoid “unused” lint when the field is only structurally referenced.
#[allow(dead_code)]
fn _link_function_def(_: &StmtFunctionDef) -> TextRange {
    TextRange::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tyc_syntax::preprocess::preprocess;

    fn prep_parse(src: &str) -> ModModule {
        let prep = preprocess(src);
        tyc_syntax::parse_module(&prep.python_source)
            .unwrap()
            .into_syntax()
    }

    /// Extract, rewrite, and emit; returns `(rewrite count, emitted Python)`.
    fn rewrite(src: &str) -> (usize, String) {
        rewrite_with(src, &TypeFacts::default())
    }

    fn rewrite_with(src: &str, external: &TypeFacts) -> (usize, String) {
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let (rewrites, _) = rewrite_builtin_extension_calls_with_facts(&mut m, &registry, external);
        (rewrites, tyc_emit::emit_python(&m))
    }

    #[test]
    fn review_contextual_lambda_and_pattern_captures() {
        let (_, emitted) = rewrite(
            r###"from typing import Callable
extend str:
    def shout(self) -> str:
        return self.upper()
let f: Callable[[str], str] = lambda x: x.shout()
let values: list[str] = ["ok"]
match values:
    case [item]:
        print(item.shout())
"###,
        );
        assert!(
            emitted.contains("__typhon_ext_str__shout__(x)"),
            "{emitted}"
        );
        assert!(
            emitted.contains("__typhon_ext_str__shout__(item)"),
            "{emitted}"
        );
    }

    const SLUG: &str = "extend str:\n    def slug(self) -> str:\n        return self.lower()\n\n";

    #[test]
    fn extract_promotes_str_extension_to_free_function() {
        let src = "extend str:\n    def shout(self) -> str:\n        return self.upper()\n";
        let mut m = prep_parse(src);
        let (registry, stats) = extract_builtin_extensions(&mut m);
        assert_eq!(stats.blocks, 1);
        assert_eq!(stats.methods, 1);
        assert!(registry.contains_key("str"));
        assert_eq!(
            registry["str"].get("shout").map(String::as_str),
            Some("__typhon_ext_str__shout__")
        );
        // The stub class is gone from the AST.
        for stmt in &m.body {
            if let Stmt::ClassDef(c) = stmt {
                assert!(
                    !c.name.as_str().starts_with(STUB_PREFIX),
                    "stub class must be removed; saw {}",
                    c.name.as_str()
                );
            }
        }
        // The free function is emitted.
        assert!(m.body.iter().any(|s| matches!(
            s, Stmt::FunctionDef(f) if f.name.as_str() == "__typhon_ext_str__shout__"
        )));
    }

    #[test]
    fn rewrite_call_when_receiver_has_str_annotation() {
        let src = "extend str:\n    def shout(self) -> str:\n        return self.upper()\n\n\
                   greeting: str = \"hi\"\nprint(greeting.shout())\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(rewrites, 1);
        let out = tyc_emit::emit_python(&m);
        assert!(
            out.contains("__typhon_ext_str__shout__(greeting)"),
            "expected call-site rewrite; got:\n{out}"
        );
    }

    #[test]
    fn rewrite_types_field_literal_and_chain_receivers() {
        let src =
            "extend str:\n    def slug(self) -> str:\n        return self.strip().lower()\n\n\
class Post:\n    title: str\n\n\
impl Post:\n    def key(self) -> str:\n        return self.title.slug()\n\n\
def f(t: str, p: Post) -> str:\n    return t.strip().slug() + \"Lit X\".slug() + p.title.slug()\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(rewrites, 4);
        let out = tyc_emit::emit_python(&m);
        assert!(
            out.contains("__typhon_ext_str__slug__(self.title)"),
            "got:\n{out}"
        );
        assert!(
            out.contains("__typhon_ext_str__slug__(t.strip())"),
            "got:\n{out}"
        );
        assert!(
            out.contains("__typhon_ext_str__slug__(\"Lit X\")"),
            "got:\n{out}"
        );
        assert!(
            out.contains("__typhon_ext_str__slug__(p.title)"),
            "got:\n{out}"
        );
    }

    #[test]
    fn rewrite_skips_unannotated_receiver() {
        // No annotation → fallback to native attribute access, which
        // raises AttributeError at runtime. The rewrite must NOT fire.
        let src = "extend str:\n    def shout(self) -> str:\n        return self.upper()\n\n\
                   greeting = object()\nprint(greeting.shout())\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(rewrites, 0);
    }

    #[test]
    fn rewrite_handles_parameter_annotation() {
        let src = "extend str:\n    def shout(self) -> str:\n        return self.upper()\n\n\
                   def greet(name: str) -> str:\n    return name.shout()\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(rewrites, 1);
        let out = tyc_emit::emit_python(&m);
        assert!(
            out.contains("__typhon_ext_str__shout__(name)"),
            "got:\n{out}"
        );
    }

    #[test]
    fn rewrite_handles_generic_list_annotation() {
        let src = "extend list:\n    def head(self) -> int:\n        return self[0]\n\n\
                   xs: list[int] = [1, 2, 3]\nprint(xs.head())\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(rewrites, 1);
    }

    #[test]
    fn rewrite_descends_into_binop_and_list_expressions() {
        // Coverage for nested expression contexts: a call inside a binop
        // and a call inside a list literal must both be rewritten.
        let src = "extend str:\n    def shout(self) -> str:\n        return self.upper()\n\n\
                   def f(name: str) -> str:\n    return name.shout() + \"!\"\n\n\
                   def g(name: str) -> list[str]:\n    return [name.shout()]\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(
            rewrites, 2,
            "expected both nested calls to be rewritten; got {rewrites}"
        );
    }

    #[test]
    fn rewrite_descends_into_for_while_with_try_match() {
        // The receiver is a parameter typed `str`, so the rewrite should
        // fire from every block-bearing statement variant.
        let src = "extend str:\n    def shout(self) -> str:\n        return self.upper()\n\n\
                   def run(name: str) -> None:\n    \
                   for _ in [1]:\n        print(name.shout())\n    \
                   while False:\n        print(name.shout())\n    \
                   with open('x') as _:\n        print(name.shout())\n    \
                   try:\n        print(name.shout())\n    except Exception:\n        \
                       print(name.shout())\n    \
                   match name:\n        case _:\n            print(name.shout())\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(
            rewrites, 6,
            "for/while/with/try-body/try-except/match should all rewrite; got {rewrites}"
        );
    }

    #[test]
    fn rewrite_picks_up_function_local_annotated_binding() {
        // A local `let s: str = ...` declaration inside a function body
        // must populate the local env in time for a subsequent call.
        let src = "extend str:\n    def shout(self) -> str:\n        return self.upper()\n\n\
                   def f() -> str:\n    s: str = \"hi\"\n    return s.shout()\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(rewrites, 1);
    }

    #[test]
    fn rewrite_does_not_leak_outer_annotation_into_function_scope() {
        // Module-level `name: str` is shadowed by a function-local
        // rebinding (no annotation, different runtime type). The
        // function must NOT inherit the outer annotation.
        let src = "extend str:\n    def shout(self) -> str:\n        return self.upper()\n\n\
                   name: str = \"global\"\n\n\
                   def f() -> str:\n    name = object()\n    return name.shout()\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(
            rewrites, 0,
            "function scope must not inherit outer module annotation; got {rewrites}"
        );
    }

    #[test]
    fn rewrite_handles_keyword_only_parameter_annotation() {
        let src = "extend str:\n    def shout(self) -> str:\n        return self.upper()\n\n\
                   def f(*, name: str) -> str:\n    return name.shout()\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(rewrites, 1);
    }

    #[test]
    fn rewrite_visits_elif_test_expression() {
        let src = "extend str:\n    def is_empty(self) -> bool:\n        return len(self) == 0\n\n\
                   def f(a: str, b: str) -> int:\n    \
                   if False:\n        return 0\n    elif b.is_empty():\n        return 1\n    \
                   return 2\n";
        let mut m = prep_parse(src);
        let (registry, _) = extract_builtin_extensions(&mut m);
        let rewrites = rewrite_builtin_extension_calls(&mut m, &registry);
        assert_eq!(rewrites, 1);
    }

    // ── receivers typed from declarations ───────────────────────────────

    #[test]
    fn rewrite_call_receiver_from_function_return_type() {
        let src = format!(
            "{SLUG}def make() -> str:\n    return \"A B\"\n\n\
             def main() -> None:\n    print(make().slug())\n"
        );
        let (rewrites, out) = rewrite(&src);
        assert_eq!(rewrites, 1, "got:\n{out}");
        assert!(
            out.contains("__typhon_ext_str__slug__(make())"),
            "got:\n{out}"
        );
    }

    #[test]
    fn rewrite_call_receiver_from_impl_method_return_type() {
        let src = format!(
            "{SLUG}class Post:\n    title: str\n\n\
             impl Post:\n    def url(self) -> str:\n        return self.title\n    \
             def both(self) -> str:\n        return self.url().slug()\n\n\
             def main(p: Post) -> None:\n    print(p.url().slug())\n"
        );
        let (rewrites, out) = rewrite(&src);
        assert_eq!(rewrites, 2, "got:\n{out}");
        assert!(
            out.contains("__typhon_ext_str__slug__(self.url())"),
            "got:\n{out}"
        );
        assert!(
            out.contains("__typhon_ext_str__slug__(p.url())"),
            "got:\n{out}"
        );
    }

    #[test]
    fn rewrite_review_reproduction_attribute_and_call_receivers() {
        // The three shapes from the 2026-09-30 release-readiness review.
        let src = "extend str:\n    def slug(self) -> str:\n        \
                   return self.lower().replace(\" \", \"-\")\n\
                   class Post:\n    title: str\n\
                   impl Post:\n    def url(self) -> str:\n        return self.title.slug()\n\
                   def make() -> str:\n    return \"A B\"\n\
                   def main() -> None:\n    let p: Post = Post(title=\"Hello World\")\n    \
                   print(p.url(), make().slug(), p.title.slug())\n\
                   main()\n";
        let (rewrites, out) = rewrite(src);
        assert_eq!(rewrites, 3, "got:\n{out}");
        assert!(
            out.contains("__typhon_ext_str__slug__(self.title)"),
            "got:\n{out}"
        );
        assert!(
            out.contains("__typhon_ext_str__slug__(make())"),
            "got:\n{out}"
        );
        assert!(
            out.contains("__typhon_ext_str__slug__(p.title)"),
            "got:\n{out}"
        );
    }

    #[test]
    fn rewrite_chained_extension_calls() {
        let src = "extend str:\n    def slug(self) -> str:\n        return self.lower()\n    \
                   def shout(self) -> str:\n        return self.upper()\n\n\
                   def f(t: str) -> str:\n    return t.slug().shout() + \"a b\".shout().slug()\n";
        let (rewrites, out) = rewrite(src);
        assert_eq!(rewrites, 4, "got:\n{out}");
        assert!(
            out.contains("__typhon_ext_str__shout__(__typhon_ext_str__slug__(t))"),
            "inner call must be lowered and nested as the outer receiver; got:\n{out}"
        );
        assert!(
            out.contains("__typhon_ext_str__slug__(__typhon_ext_str__shout__(\"a b\"))"),
            "got:\n{out}"
        );
    }

    #[test]
    fn rewrite_subscript_receivers() {
        let src = format!(
            "{SLUG}class Post:\n    tags: list[str]\n    meta: dict[str, str]\n    pair: tuple[int, str]\n\n\
             impl Post:\n    def first(self, i: int) -> str:\n        \
             return self.tags[i].slug() + self.meta[\"k\"].slug() + self.pair[1].slug() + self.pair[-1].slug()\n\n\
             def f(xs: list[str], d: dict[str, str], t: tuple[str, ...], p: tuple[int, str]) -> str:\n    \
             return xs[0].slug() + d[\"k\"].slug() + t[3].slug() + xs[1:][0].slug() + p[0].slug()\n"
        );
        let (rewrites, out) = rewrite(&src);
        // `p[0]` is an `int` and must stay untouched.
        assert_eq!(rewrites, 8, "got:\n{out}");
        assert!(
            out.contains("__typhon_ext_str__slug__(self.tags[i])"),
            "got:\n{out}"
        );
        assert!(
            out.contains("__typhon_ext_str__slug__(self.pair[-1])"),
            "got:\n{out}"
        );
        assert!(
            out.contains("__typhon_ext_str__slug__(xs[1:][0])"),
            "got:\n{out}"
        );
        assert!(out.contains("p[0].slug()"), "got:\n{out}");
    }

    #[test]
    fn rewrite_skips_async_call_receiver_but_types_awaited_value() {
        let src = format!(
            "{SLUG}async def make() -> str:\n    return \"A B\"\n\n\
             async def main() -> None:\n    print(make().slug())\n    \
             print((await make()).slug())\n"
        );
        let (rewrites, out) = rewrite(&src);
        assert_eq!(rewrites, 1, "got:\n{out}");
        assert!(
            out.contains("make().slug()"),
            "coroutine receiver must stay; got:\n{out}"
        );
        assert!(
            out.contains("__typhon_ext_str__slug__(await make())")
                || out.contains("__typhon_ext_str__slug__((await make()))"),
            "got:\n{out}"
        );
    }

    #[test]
    fn rewrite_inferred_local_and_module_bindings() {
        let src = format!(
            "{SLUG}def make() -> str:\n    return \"A B\"\n\n\
             def f() -> str:\n    s = make()\n    t = \"x y\"\n    u = s + t\n    \
             return s.slug() + t.slug() + u.slug()\n\n\
             g = make()\nprint(g.slug())\n"
        );
        let (rewrites, out) = rewrite(&src);
        assert_eq!(rewrites, 4, "got:\n{out}");
        assert!(out.contains("__typhon_ext_str__slug__(u)"), "got:\n{out}");
        assert!(out.contains("__typhon_ext_str__slug__(g)"), "got:\n{out}");
    }

    #[test]
    fn rewrite_for_target_and_comprehension_variable() {
        let src = format!(
            "{SLUG}def f(titles: list[str], d: dict[str, str]) -> list[str]:\n    \
             for t in titles:\n        print(t.slug())\n    \
             for k, v in d.items():\n        print(k.slug(), v.slug())\n    \
             for i, t2 in enumerate(titles):\n        print(t2.slug())\n    \
             return [t3.slug() for t3 in titles if t3.slug()]\n"
        );
        let (rewrites, out) = rewrite(&src);
        assert_eq!(rewrites, 6, "got:\n{out}");
        assert!(out.contains("__typhon_ext_str__slug__(t2)"), "got:\n{out}");
        assert!(out.contains("__typhon_ext_str__slug__(t3)"), "got:\n{out}");
    }

    #[test]
    fn rewrite_inherited_field_property_and_generic_substitution() {
        let src = format!(
            "{SLUG}class Base:\n    title: str\n\nclass Sub(Base):\n    extra: int\n\n\
             impl Sub:\n    @property\n    def label(self) -> str:\n        return self.title\n    \
             @staticmethod\n    def make() -> str:\n        return \"a b\"\n\n\
             class Box[T]:\n    value: T\n\n\
             def f(s: Sub, b: Box[str], n: Box[int]) -> str:\n    \
             return s.title.slug() + s.label.slug() + Sub.make().slug() + b.value.slug() + \
             Sub(title=\"a\", extra=1).title.slug() + n.value.slug()\n"
        );
        let (rewrites, out) = rewrite(&src);
        // `n.value` is an `int` and must stay untouched.
        assert_eq!(rewrites, 5, "got:\n{out}");
        assert!(
            out.contains("__typhon_ext_str__slug__(s.label)"),
            "got:\n{out}"
        );
        assert!(
            out.contains("__typhon_ext_str__slug__(Sub.make())"),
            "got:\n{out}"
        );
        assert!(
            out.contains("__typhon_ext_str__slug__(b.value)"),
            "got:\n{out}"
        );
        assert!(out.contains("n.value.slug()"), "got:\n{out}");
    }

    #[test]
    fn rewrite_int_list_dict_receivers_of_every_shape() {
        let src = "extend int:\n    def twice(self) -> int:\n        return self * 2\n\
                   extend list:\n    def total(self) -> int:\n        return len(self)\n\
                   extend dict:\n    def size(self) -> int:\n        return len(self)\n\
                   class Bag:\n    n: int\n    xs: list[int]\n    d: dict[str, int]\n\
                   impl Bag:\n    def count(self) -> int:\n        return self.n\n\
                   def make_list() -> list[int]:\n    return [1]\n\
                   def f(b: Bag) -> int:\n    \
                   return b.n.twice() + b.count().twice() + b.xs.total() + make_list().total() + \
                   b.d.size() + b.xs[0].twice() + b.d[\"k\"].twice() + len(b.xs).twice()\n";
        let (rewrites, out) = rewrite(src);
        assert_eq!(rewrites, 8, "got:\n{out}");
        assert!(
            out.contains("__typhon_ext_int__twice__(b.count())"),
            "got:\n{out}"
        );
        assert!(
            out.contains("__typhon_ext_list__total__(make_list())"),
            "got:\n{out}"
        );
        assert!(
            out.contains("__typhon_ext_dict__size__(b.d)"),
            "got:\n{out}"
        );
        assert!(
            out.contains("__typhon_ext_int__twice__(len(b.xs))"),
            "got:\n{out}"
        );
    }

    #[test]
    fn rewrite_uses_external_facts_for_imported_names() {
        let src = format!(
            "{SLUG}from textutil import Post, make\nimport textutil\n\n\
             def f(p: Post, q: textutil.Post) -> str:\n    \
             return p.title.slug() + make().slug() + textutil.make().slug() + \
             q.title.slug() + p.url().slug() + textutil.Post(title=\"a\").title.slug()\n"
        );
        let mut provider = TypeFacts::default();
        provider
            .functions
            .insert("make".to_owned(), StaticType::simple("str"));
        let mut post = ClassFacts::default();
        post.fields
            .insert("title".to_owned(), StaticType::simple("str"));
        post.methods
            .insert("url".to_owned(), StaticType::simple("str"));
        provider.classes.insert("Post".to_owned(), post);
        let mut external = TypeFacts::default();
        external.import_name(&provider, "Post", "Post");
        external.import_name(&provider, "make", "make");
        external.modules.insert("textutil".to_owned(), provider);
        let (rewrites, out) = rewrite_with(&src, &external);
        assert_eq!(rewrites, 6, "got:\n{out}");
        assert!(
            out.contains("__typhon_ext_str__slug__(textutil.make())"),
            "got:\n{out}"
        );
        assert!(
            out.contains("__typhon_ext_str__slug__(q.title)"),
            "got:\n{out}"
        );
        assert!(
            out.contains("__typhon_ext_str__slug__(textutil.Post(title=\"a\").title)"),
            "got:\n{out}"
        );
    }

    #[test]
    fn rewrite_chained_imported_extension_uses_external_return_type() {
        // `slug` is declared in another module: the registry entry comes
        // from the caller and the lifted function's return type from the
        // external facts, so `t.slug().shout()` still types.
        let src = "extend str:\n    def shout(self) -> str:\n        return self.upper()\n\n\
                   def f(t: str) -> str:\n    return t.slug().shout()\n";
        let mut m = prep_parse(src);
        let (mut registry, _) = extract_builtin_extensions(&mut m);
        registry
            .entry("str".to_owned())
            .or_default()
            .insert("slug".to_owned(), "__typhon_ext_str__slug__".to_owned());
        let mut external = TypeFacts::default();
        external.functions.insert(
            "__typhon_ext_str__slug__".to_owned(),
            StaticType::simple("str"),
        );
        let (rewrites, used) =
            rewrite_builtin_extension_calls_with_facts(&mut m, &registry, &external);
        let out = tyc_emit::emit_python(&m);
        assert_eq!(rewrites, 2, "got:\n{out}");
        assert!(used.contains("__typhon_ext_str__slug__"));
        assert!(
            out.contains("__typhon_ext_str__shout__(__typhon_ext_str__slug__(t))"),
            "got:\n{out}"
        );
    }

    #[test]
    fn rewrite_refinement_is_dropped_at_branch_join_and_loop_head() {
        let src = format!(
            "{SLUG}def unknown() -> object:\n    return 1\n\n\
             def f(c: bool) -> None:\n    x = unknown()\n    \
             if c:\n        x = \"a\"\n        print(x.slug())\n    \
             print(x.slug())\n    \
             y = \"b\"\n    for _ in range(2):\n        print(y.slug())\n        y = unknown()\n    \
             z = \"c\"\n    while c:\n        print(z.slug())\n    print(z.slug())\n"
        );
        let (rewrites, out) = rewrite(&src);
        // Rewritten: `x` inside the branch, `z` inside and after the loop
        // (never reassigned). Kept: `x` after the join, `y` inside the
        // loop that reassigns it.
        assert_eq!(rewrites, 3, "got:\n{out}");
        assert!(out.contains("print(x.slug())"), "got:\n{out}");
        assert!(out.contains("print(y.slug())"), "got:\n{out}");
        assert!(out.contains("__typhon_ext_str__slug__(z)"), "got:\n{out}");
    }

    #[test]
    fn rewrite_module_level_annotated_global_inside_function() {
        let src = format!(
            "{SLUG}NAME: str = \"a b\"\n\n\
             def f() -> str:\n    return NAME.slug()\n\n\
             def g() -> str:\n    NAME = object()\n    return NAME.slug()\n"
        );
        let (rewrites, out) = rewrite(&src);
        assert_eq!(rewrites, 1, "got:\n{out}");
        assert!(
            out.contains("__typhon_ext_str__slug__(NAME)"),
            "got:\n{out}"
        );
    }

    #[test]
    fn rewrite_local_binding_masks_module_function_and_class() {
        let src = format!(
            "{SLUG}def make() -> str:\n    return \"a\"\n\nclass Post:\n    title: str\n\n\
             def f(make: object, Post: object) -> None:\n    print(make().slug())\n    \
             print(Post().title.slug())\n\n\
             def g() -> None:\n    def make() -> int:\n        return 1\n    print(make().slug())\n"
        );
        let (rewrites, out) = rewrite(&src);
        assert_eq!(rewrites, 0, "got:\n{out}");
    }

    #[test]
    fn rewrite_nullable_receiver_reads_as_base_type() {
        // `t: str?` lowers to `str | None`; the checker only lets the
        // call through once `t` is narrowed, so the receiver is a `str`.
        let src = format!(
            "{SLUG}def f(t: str?) -> str:\n    if t is None:\n        return \"\"\n    \
             return t.slug()\n"
        );
        let (rewrites, out) = rewrite(&src);
        assert_eq!(rewrites, 1, "got:\n{out}");
    }

    #[test]
    fn rewrite_operator_and_builtin_method_receivers() {
        let src = format!(
            "{SLUG}def f(a: str, b: str, n: int) -> str:\n    \
             return (a + \" \" + b).slug() + (a * n).slug() + a.split()[0].slug() + \
             (a if n else b).slug() + str(n).slug() + f\"{{a}}\".slug()\n"
        );
        let (rewrites, out) = rewrite(&src);
        assert_eq!(rewrites, 6, "got:\n{out}");
    }

    #[test]
    fn rewrite_min_max_abs_sum_round_and_dict_get_receivers() {
        let src = "extend str:\n    def slug(self) -> str:\n        return self.lower()\n\n\
                   extend int:\n    def twice(self) -> int:\n        return self * 2\n\n\
                   def f(xs: list[str], ns: list[int], d: dict[str, str], a: int, b: int) -> str:\n    \
                   v = d.get(\"k\")\n    \
                   n = abs(a).twice() + sum(ns).twice() + max(a, b).twice() + round(2.5).twice()\n    \
                   if v is None:\n        return str(n)\n    \
                   return min(xs).slug() + max(\"a b\", \"c d\").slug() + v.slug() + \
                   d.setdefault(\"k\", \"x\").slug() + str(n)\n";
        let (rewrites, out) = rewrite(src);
        assert_eq!(rewrites, 8, "got:\n{out}");
        assert!(
            out.contains("__typhon_ext_int__twice__(sum(ns))"),
            "got:\n{out}"
        );
        assert!(out.contains("__typhon_ext_str__slug__(v)"), "got:\n{out}");
        assert!(
            out.contains("__typhon_ext_str__slug__(min(xs))"),
            "got:\n{out}"
        );
    }

    #[test]
    fn rewrite_leaves_unknown_receivers_alone() {
        let src = format!(
            "{SLUG}def unknown() -> object:\n    return 1\n\n\
             class Post:\n    title: str\n\n\
             def f(u: object, xs: list[object], x: str | int) -> None:\n    \
             print(unknown().slug())\n    print(u.slug())\n    print(xs[0].slug())\n    \
             print(x.slug())\n    print(Post.title.slug())\n"
        );
        let (rewrites, out) = rewrite(&src);
        assert_eq!(rewrites, 0, "got:\n{out}");
    }

    #[test]
    fn collect_module_type_facts_records_functions_classes_and_lifted_names() {
        let src = "extend str:\n    def slug(self) -> str:\n        return self\n\n\
                   def make() -> str:\n    return \"a\"\n\nasync def fetch() -> str:\n    return \"a\"\n\n\
                   class Post(Base):\n    title: str\n\nimpl Post:\n    def url(self) -> str:\n        return \"\"\n    \
                   @property\n    def label(self) -> str:\n        return \"\"\n    \
                   async def load(self) -> bytes:\n        return b\"\"\n    \
                   @classmethod\n    def build(cls) -> Post:\n        return Post(title=\"\")\n\n\
                   rebound = 1\ndef rebound() -> str:\n    return \"\"\n";
        let m = prep_parse(src);
        let facts = collect_module_type_facts(&m);
        assert_eq!(
            facts.functions.get("make"),
            Some(&StaticType::simple("str"))
        );
        assert_eq!(
            facts.functions.get("__typhon_ext_str__slug__"),
            Some(&StaticType::simple("str"))
        );
        assert_eq!(
            facts.async_functions.get("fetch"),
            Some(&StaticType::simple("str"))
        );
        assert!(!facts.functions.contains_key("fetch"));
        assert!(!facts.functions.contains_key("rebound"));
        let post = &facts.classes["Post"];
        assert_eq!(post.bases, vec!["Base".to_owned()]);
        assert_eq!(post.fields.get("title"), Some(&StaticType::simple("str")));
        assert_eq!(post.fields.get("label"), Some(&StaticType::simple("str")));
        assert_eq!(post.methods.get("url"), Some(&StaticType::simple("str")));
        assert_eq!(
            post.async_methods.get("load"),
            Some(&StaticType::simple("bytes"))
        );
        assert_eq!(
            post.static_methods.get("build"),
            Some(&StaticType::simple("Post"))
        );
    }

    #[test]
    fn static_type_from_annotation_reads_generics_optionals_and_unions() {
        let parse = |s: &str| {
            let m = prep_parse(&format!("x: {s} = 0\n"));
            let Stmt::AnnAssign(a) = &m.body[0] else {
                panic!()
            };
            StaticType::from_annotation(&a.annotation)
        };
        assert_eq!(parse("str"), Some(StaticType::simple("str")));
        assert_eq!(
            parse("dict[str, list[int]]"),
            Some(StaticType::with_args(
                "dict",
                vec![
                    StaticType::simple("str"),
                    StaticType::with_args("list", vec![StaticType::simple("int")])
                ]
            ))
        );
        assert_eq!(parse("str?"), Some(StaticType::simple("str")));
        assert_eq!(parse("Optional[str]"), Some(StaticType::simple("str")));
        assert_eq!(parse("str | int"), None);
        assert_eq!(parse("mod.Post"), Some(StaticType::simple("mod.Post")));
        assert_eq!(
            parse("tuple[str, ...]"),
            Some(StaticType::with_args(
                "tuple",
                vec![StaticType::simple("str"), StaticType::simple("...")]
            ))
        );
        assert_eq!(
            parse("Callable[[int], str]").and_then(|t| callable_return(&t)),
            Some(StaticType::simple("str"))
        );
    }
}
