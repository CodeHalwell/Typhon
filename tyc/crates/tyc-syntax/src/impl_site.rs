//! Which `impl` / `extend` methods have to be defined where their block is.
//!
//! `merge_impl_blocks` moves every method of a local `impl Target:` block
//! into `Target`'s class body. A method's *def-time* expressions — its
//! decorators and parameter defaults — are then evaluated when the class
//! statement runs, not where the `impl` block sits. A name bound between the
//! class and the block (`let DEFAULT_GREETING = …`, a decorator `def`) does
//! not exist yet at the class, and importing the module raised `NameError` on
//! both execution surfaces (W7-06).
//!
//! Such a method is instead defined at the `impl` site and attached to the
//! class there (`Target.m = __typhon_extend_Target__m`, the same lowering a
//! cross-module `extend` already uses). Everything else keeps the merged
//! class — a method that ran correctly before is emitted exactly as before.
//!
//! A method moves only when all of these hold:
//! - a def-time name is bound at module level strictly between the class and
//!   the block, is not bound before the class, and is not a builtin — i.e.
//!   hoisting it raised `NameError`, so no program that ran changes;
//! - it does not depend on class-body semantics that an attribute assignment
//!   cannot reproduce: dunder methods (`__post_init__`, `__init_subclass__`,
//!   `__eq__`'s implicit `__hash__ = None`, …), private-name mangling
//!   (`self.__x`), `cached_property` (needs `__set_name__`), `abstractmethod`
//!   (`__abstractmethods__` is fixed at class creation), and def-time names
//!   that resolve in the class namespace (`@x.setter`, `f=helper`);
//! - it does not hit a gap in how `tyc run` dispatches a function stored as
//!   a class attribute (shared with cross-module `extend`): a `@property` or
//!   `@classmethod` (read through the class), a method of a class with a
//!   subclass defined before the block (that subclass never sees the
//!   attribute), a method a subclass in this module also defines (its
//!   `super().m()` misses the attribute), or a method the class may inherit
//!   from a base (the VM resolves the inherited one first). Those keep the
//!   merged placement until the VM closes the gap, so the two execution
//!   surfaces never disagree. A builtin exception base (`class
//!   AppError(Exception)`) has a known member list, so a method it does not
//!   define is not inherited from it.
//!
//! A method that has to stay merged although a def-time name is bound only
//! after the class still raises `NameError` when the module is imported.
//! [`merged_name_errors`] lists those, so the resolver can report them at
//! check time (`tyc::impl_forward_reference`) instead of the program
//! crashing on import. It leaves out every case where the name might be
//! bound before the class after all: a `from … import *` above the class, a
//! `global NAME` in any function (one may run before the class), or a
//! module that writes its namespace dynamically (`globals()`, `exec`, …).
//!
//! The planner lives in `tyc-syntax` because both `tyc-desugar` (which
//! places the methods) and `tyc-resolve` (which reports the crashes) must
//! make exactly the same decision.

use std::collections::{HashMap, HashSet};

use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_ast::{Decorator, ExceptHandler, Expr, Operator, Parameter, Stmt};
use ruff_text_size::{Ranged, TextRange};

/// Python 3.13 builtins. A def-time name that resolves to one of these at the
/// class site did not crash there, so moving the method would change a
/// program that ran.
#[rustfmt::skip]
const PY_BUILTINS: &[&str] = &[
    "ArithmeticError", "AssertionError", "AttributeError", "BaseException",
    "BaseExceptionGroup", "BlockingIOError", "BrokenPipeError", "BufferError", "BytesWarning",
    "ChildProcessError", "ConnectionAbortedError", "ConnectionError", "ConnectionRefusedError",
    "ConnectionResetError", "DeprecationWarning", "EOFError", "Ellipsis", "EncodingWarning",
    "EnvironmentError", "Exception", "ExceptionGroup", "False", "FileExistsError",
    "FileNotFoundError", "FloatingPointError", "FutureWarning", "GeneratorExit", "IOError",
    "ImportError", "ImportWarning", "IndentationError", "IndexError", "InterruptedError",
    "IsADirectoryError", "KeyError", "KeyboardInterrupt", "LookupError", "MemoryError",
    "ModuleNotFoundError", "NameError", "None", "NotADirectoryError", "NotImplemented",
    "NotImplementedError", "OSError", "OverflowError", "PendingDeprecationWarning",
    "PermissionError", "ProcessLookupError", "PythonFinalizationError", "RecursionError",
    "ReferenceError", "ResourceWarning", "RuntimeError", "RuntimeWarning", "StopAsyncIteration",
    "StopIteration", "SyntaxError", "SyntaxWarning", "SystemError", "SystemExit", "TabError",
    "TimeoutError", "True", "TypeError", "UnboundLocalError", "UnicodeDecodeError",
    "UnicodeEncodeError", "UnicodeError", "UnicodeTranslateError", "UnicodeWarning",
    "UserWarning", "ValueError", "Warning", "ZeroDivisionError", "__build_class__", "__debug__",
    "__doc__", "__import__", "__name__", "abs", "aiter", "all", "anext", "any", "ascii", "bin",
    "bool", "breakpoint", "bytearray", "bytes", "callable", "chr", "classmethod", "compile",
    "complex", "copyright", "credits", "delattr", "dict", "dir", "divmod", "enumerate", "eval",
    "exec", "exit", "filter", "float", "format", "frozenset", "getattr", "globals", "hasattr",
    "hash", "help", "hex", "id", "input", "int", "isinstance", "issubclass", "iter", "len",
    "license", "list", "locals", "map", "max", "memoryview", "min", "next", "object", "oct",
    "open", "ord", "pow", "print", "property", "quit", "range", "repr", "reversed", "round",
    "set", "setattr", "slice", "sorted", "staticmethod", "str", "sum", "super", "tuple", "type",
    "vars", "zip",
];

/// Module-level binding facts the per-method decision reads.
pub struct ImplSitePlanner<'a> {
    body: &'a [Stmt],
    /// Top-level index of each (non-`impl`) class, first definition wins.
    class_index: HashMap<&'a str, usize>,
    /// Names each top-level statement binds.
    bound: Vec<Vec<&'a str>>,
    /// Index of the first top-level `from … import *`, if any.
    first_star_import: Option<usize>,
}

/// Where one `impl` method goes, and why.
enum Decision {
    /// Merged into the class body, which evaluates its def-time names fine.
    Merge,
    /// Defined at the `impl` block and attached to the class there.
    AtImplSite,
    /// Merged although `late` (bound only after the class) is read at def
    /// time: importing the module raises `NameError`. `why` says what keeps
    /// the method in the class body.
    MergedLate {
        late: Vec<(String, TextRange)>,
        why: &'static str,
    },
}

/// An `impl` method that stays in its class body although one of its
/// decorators or parameter defaults reads a name bound only after the
/// class: importing the module raises `NameError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedNameError {
    /// The late-bound name.
    pub name: String,
    /// Where the method reads it (a decorator or a parameter default).
    pub range: TextRange,
    /// The class the method is merged into.
    pub class: String,
    /// The method.
    pub method: String,
    /// Why the method cannot be defined at its `impl` block instead.
    pub why: &'static str,
    /// Top-level index of the statement that first binds `name` after the
    /// class.
    pub bound_at: usize,
}

impl<'a> ImplSitePlanner<'a> {
    pub fn new(body: &'a [Stmt], impl_prefix: &str) -> Self {
        let mut class_index = HashMap::new();
        let mut bound = Vec::with_capacity(body.len());
        let mut first_star_import = None;
        for (i, stmt) in body.iter().enumerate() {
            if let Stmt::ClassDef(c) = stmt {
                let name = c.name.as_str();
                if !name.starts_with(impl_prefix) {
                    class_index.entry(name).or_insert(i);
                }
            }
            let mut names = Vec::new();
            let mut star = false;
            stmt_bound_names(stmt, &mut names, &mut star);
            if star && first_star_import.is_none() {
                first_star_import = Some(i);
            }
            bound.push(names);
        }
        Self {
            body,
            class_index,
            bound,
            first_star_import,
        }
    }

    /// Whether attaching method `name` to `target` as a class attribute at
    /// top-level index `impl_idx` would hit a VM dispatch gap through a
    /// module-level subclass: one defined before the attachment, or one (at
    /// any depth) that defines `name` itself.
    fn subclass_blocks_attachment(&self, target: &str, impl_idx: usize, name: &str) -> bool {
        let mut family: HashSet<&str> = HashSet::from([target]);
        loop {
            let before = family.len();
            for (i, stmt) in self.body.iter().enumerate() {
                let Stmt::ClassDef(c) = stmt else { continue };
                let inherits = c.arguments.as_ref().is_some_and(|args| {
                    args.args.iter().any(|b| match b {
                        Expr::Name(n) => family.contains(n.id.as_str()),
                        Expr::Attribute(a) => family.contains(a.attr.as_str()),
                        _ => false,
                    })
                });
                if !inherits || !family.insert(c.name.as_str()) {
                    continue;
                }
                let overrides = c
                    .body
                    .iter()
                    .any(|m| matches!(m, Stmt::FunctionDef(f) if f.name.as_str() == name));
                if i < impl_idx || overrides {
                    return true;
                }
            }
            if family.len() == before {
                return false;
            }
        }
    }

    /// Whether `target` may inherit a member called `name` from a base: a
    /// module-level base (at any depth) whose merged body binds it, or any
    /// base this module does not define (its members are unknown).
    fn may_inherit(
        &self,
        target: &str,
        name: &str,
        class_scopes: &HashMap<String, HashSet<String>>,
        seen: &mut HashSet<String>,
    ) -> bool {
        if !seen.insert(target.to_owned()) {
            return false;
        }
        let Some(&idx) = self.class_index.get(target) else {
            return true;
        };
        let Stmt::ClassDef(c) = &self.body[idx] else {
            return true;
        };
        let Some(args) = c.arguments.as_ref() else {
            return false;
        };
        args.args.iter().any(|base| match base {
            Expr::Name(n) if n.id.as_str() == "object" => false,
            // A builtin exception's members are a fixed list.
            Expr::Name(n)
                if !self.class_index.contains_key(n.id.as_str())
                    && crate::builtin_exceptions::is_builtin_exception(n.id.as_str()) =>
            {
                crate::builtin_exceptions::has_attr(n.id.as_str(), name)
            }
            Expr::Name(n) if self.class_index.contains_key(n.id.as_str()) => {
                let base_name = n.id.as_str();
                class_scopes
                    .get(base_name)
                    .is_some_and(|scope| scope.contains(name))
                    || self.may_inherit(base_name, name, class_scopes, seen)
            }
            _ => true,
        })
    }

    /// Whether `method`, from an `impl` block at top-level index `impl_idx`
    /// targeting the local class `target`, must be defined at the block
    /// instead of inside the class body. `class_scopes` maps each local class
    /// to every name its merged body binds (own members plus `impl` methods).
    pub fn must_define_at_impl_site(
        &self,
        target: &str,
        impl_idx: usize,
        method: &Stmt,
        class_scopes: &HashMap<String, HashSet<String>>,
    ) -> bool {
        matches!(
            self.decide(target, impl_idx, method, class_scopes),
            Decision::AtImplSite
        )
    }

    fn decide(
        &self,
        target: &str,
        impl_idx: usize,
        method: &Stmt,
        class_scopes: &HashMap<String, HashSet<String>>,
    ) -> Decision {
        let Stmt::FunctionDef(f) = method else {
            return Decision::Merge;
        };
        let Some(&class_idx) = self.class_index.get(target) else {
            return Decision::Merge;
        };
        if class_idx >= impl_idx {
            return Decision::Merge;
        }
        if self.first_star_import.is_some_and(|s| s < class_idx) {
            return Decision::Merge;
        }
        let def_time = def_time_names(&f.decorator_list, &f.parameters);
        let early: HashSet<&str> = self.bound[..class_idx].iter().flatten().copied().collect();
        let late: HashSet<&str> = self.bound[class_idx + 1..impl_idx]
            .iter()
            .flatten()
            .copied()
            .collect();
        let late_reads: Vec<(String, TextRange)> = def_time
            .iter()
            .filter(|(n, _)| {
                late.contains(n.as_str())
                    && !early.contains(n.as_str())
                    && !PY_BUILTINS.contains(&n.as_str())
            })
            .cloned()
            .collect();
        if late_reads.is_empty() {
            return Decision::Merge;
        }
        let no_names = HashSet::new();
        let class_scope = class_scopes.get(target).unwrap_or(&no_names);
        // A def-time name the class body binds resolves there: the merged
        // method never raised.
        if def_time.iter().any(|(n, _)| class_scope.contains(n)) {
            return Decision::Merge;
        }
        let stays = |why| Decision::MergedLate {
            late: late_reads.clone(),
            why,
        };
        let name = f.name.as_str();
        if name.starts_with("__") && name.ends_with("__") {
            return stays("it is a special method, which the class needs when it is created");
        }
        if f.decorator_list.iter().any(needs_class_creation) {
            return stays(
                "its `@property` / `@classmethod` / `@cached_property` / `@abstractmethod` decorator ties it to the class body",
            );
        }
        if self.subclass_blocks_attachment(target, impl_idx, name) {
            return stays(
                "a subclass is defined before this `impl` block, or overrides the method",
            );
        }
        if self.may_inherit(target, name, class_scopes, &mut HashSet::new()) {
            return stays("the class may inherit a member of the same name from a base class");
        }
        if uses_private_name(method) {
            return stays(
                "it uses a private `__name`, which Python mangles only inside a class body",
            );
        }
        Decision::AtImplSite
    }

    /// The `NameError`s [`Self::must_define_at_impl_site`] cannot avoid for
    /// `method`: def-time names bound only after the class, read by a method
    /// that must stay in the class body. Empty when any of them might be
    /// bound before the class after all (`global_names`: names some
    /// function declares `global`; `dynamic`: the module writes its
    /// namespace through `globals()` / `exec` / …).
    fn merged_name_errors_for(
        &self,
        target: &str,
        impl_idx: usize,
        method: &Stmt,
        class_scopes: &HashMap<String, HashSet<String>>,
        global_names: &HashSet<&str>,
    ) -> Vec<MergedNameError> {
        let Decision::MergedLate { late, why } =
            self.decide(target, impl_idx, method, class_scopes)
        else {
            return Vec::new();
        };
        let Stmt::FunctionDef(f) = method else {
            return Vec::new();
        };
        let Some(&class_idx) = self.class_index.get(target) else {
            return Vec::new();
        };
        if late.iter().any(|(n, _)| global_names.contains(n.as_str())) {
            return Vec::new();
        }
        late.into_iter()
            .filter_map(|(name, range)| {
                let bound_at =
                    (class_idx + 1..impl_idx).find(|i| self.bound[*i].contains(&name.as_str()))?;
                Some(MergedNameError {
                    name,
                    range,
                    class: target.to_owned(),
                    method: f.name.as_str().to_owned(),
                    why,
                    bound_at,
                })
            })
            .collect()
    }
}

/// Every `impl` method of `body` that stays merged into its class although
/// a decorator or parameter default reads a name bound only after the class
/// — importing the module raises `NameError` (see the module docs). One
/// entry per read; a method distributed over a sealed union's variants is
/// reported once.
pub fn merged_name_errors(body: &[Stmt], impl_prefix: &str) -> Vec<MergedNameError> {
    if body.iter().any(writes_namespace_dynamically) {
        return Vec::new();
    }
    let union_aliases = collect_sealed_union_aliases(body);
    let local_classes: HashSet<&str> = body
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::ClassDef(c) if !c.name.as_str().starts_with(impl_prefix) => Some(c.name.as_str()),
            _ => None,
        })
        .collect();
    let impls: Vec<(usize, &ruff_python_ast::StmtClassDef, &str)> = body
        .iter()
        .enumerate()
        .filter_map(|(i, stmt)| match stmt {
            Stmt::ClassDef(c) => c
                .name
                .as_str()
                .strip_prefix(impl_prefix)
                .map(|target| (i, c, target)),
            _ => None,
        })
        .collect();
    if impls.is_empty() {
        return Vec::new();
    }
    let targets_of = |target: &str| -> Vec<String> {
        match union_aliases.get(target) {
            Some(variants) => variants.clone(),
            None => vec![target.to_owned()],
        }
    };
    let class_scopes = class_scopes(body, impl_prefix, &union_aliases);
    let global_names = global_declarations(body);
    let planner = ImplSitePlanner::new(body, impl_prefix);
    let mut out: Vec<MergedNameError> = Vec::new();
    for (impl_idx, block, target) in impls {
        let targets = targets_of(target);
        // Mirrors `merge_impl_blocks`: a block aimed at anything that is not
        // a local class (or a local union of them) is patched in place.
        if !targets
            .iter()
            .all(|t| local_classes.contains(t.as_str()) || union_aliases.contains_key(t))
        {
            continue;
        }
        for method in &block.body {
            for t in &targets {
                for e in planner.merged_name_errors_for(
                    t,
                    impl_idx,
                    method,
                    &class_scopes,
                    &global_names,
                ) {
                    if !out.iter().any(|o| o.range == e.range) {
                        out.push(e);
                    }
                }
            }
        }
    }
    out
}

/// Every name each local class's merged body binds: its own members plus
/// every `impl` method aimed at it (directly or through a sealed union).
pub fn class_scopes(
    body: &[Stmt],
    impl_prefix: &str,
    union_aliases: &HashMap<String, Vec<String>>,
) -> HashMap<String, HashSet<String>> {
    let mut class_scope: HashMap<String, HashSet<String>> = HashMap::new();
    for stmt in body {
        if let Stmt::ClassDef(c) = stmt {
            match c.name.as_str().strip_prefix(impl_prefix) {
                None => class_body_names(
                    &c.body,
                    class_scope.entry(c.name.as_str().to_owned()).or_default(),
                ),
                Some(target) => {
                    let targets = union_aliases
                        .get(target)
                        .cloned()
                        .unwrap_or_else(|| vec![target.to_owned()]);
                    for t in targets {
                        class_body_names(&c.body, class_scope.entry(t).or_default());
                    }
                }
            }
        }
    }
    class_scope
}

/// Names some function (at any depth) declares `global`: calling it may bind
/// the name at module level at any point, including before a class.
fn global_declarations(body: &[Stmt]) -> HashSet<&str> {
    struct V<'b>(HashSet<&'b str>);
    impl<'b> Visitor<'b> for V<'b> {
        fn visit_stmt(&mut self, s: &'b Stmt) {
            if let Stmt::Global(g) = s {
                self.0.extend(g.names.iter().map(|n| n.as_str()));
            }
            visitor::walk_stmt(self, s);
        }
    }
    let mut v = V(HashSet::new());
    for s in body {
        v.visit_stmt(s);
    }
    v.0
}

/// Whether `stmt` may write module globals by name at run time
/// (`globals()[…] = …`, `exec`, `eval`, `vars()`, `setattr` on a module,
/// `sys.modules`, `builtins` / `__builtins__`).
fn writes_namespace_dynamically(stmt: &Stmt) -> bool {
    struct V(bool);
    impl<'b> Visitor<'b> for V {
        fn visit_expr(&mut self, e: &'b Expr) {
            match e {
                Expr::Name(n)
                    if matches!(
                        n.id.as_str(),
                        "globals"
                            | "exec"
                            | "eval"
                            | "vars"
                            | "setattr"
                            | "builtins"
                            | "__builtins__"
                    ) =>
                {
                    self.0 = true
                }
                Expr::Attribute(a) if a.attr.as_str() == "modules" => self.0 = true,
                _ => visitor::walk_expr(self, e),
            }
        }
        fn visit_alias(&mut self, a: &'b ruff_python_ast::Alias) {
            if a.name.as_str() == "builtins" {
                self.0 = true;
            }
        }
    }
    let mut v = V(false);
    v.visit_stmt(stmt);
    v.0
}

/// The variant class names of a sealed-union alias value (`A | B | C`), in
/// source order; `None` for anything else (fewer than two variants, or a
/// member that is not a bare name).
pub fn collect_union_variant_names(expr: &Expr) -> Option<Vec<String>> {
    let mut names = Vec::new();
    let mut stack = vec![expr];
    while let Some(current) = stack.pop() {
        match current {
            Expr::Name(n) => names.push(n.id.as_str().to_owned()),
            // Push `right` first so the stack pops left-to-right: a
            // `BinOp` reads `A | B | C` as `BinOp(BinOp(A, B), C)` —
            // pushing left last makes the visitor descend leftmost first
            // and preserves source order in `names`. PR #129 gemini review.
            Expr::BinOp(b) if matches!(b.op, Operator::BitOr) => {
                stack.push(&b.right);
                stack.push(&b.left);
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

/// Collect every sealed-union type alias declared at module scope so
/// `impl Union:` can distribute its methods across every variant.
/// Keyed by the alias name; values are the ordered variant class names
/// as they appear in `type Union = A | B | C`. R2-3.
pub fn collect_sealed_union_aliases(body: &[Stmt]) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    for stmt in body {
        if let Stmt::TypeAlias(ta) = stmt {
            if let Expr::Name(n) = ta.name.as_ref() {
                if let Some(variants) = collect_union_variant_names(&ta.value) {
                    out.insert(n.id.as_str().to_owned(), variants);
                }
            }
        }
    }
    out
}

/// Names bound in the class body of `class_def` (fields, methods, nested
/// classes, class-level assignments).
pub fn class_body_names(body: &[Stmt], out: &mut HashSet<String>) {
    for stmt in body {
        let mut names = Vec::new();
        let mut star = false;
        stmt_bound_names(stmt, &mut names, &mut star);
        out.extend(names.into_iter().map(str::to_owned));
    }
}

/// Collect the names a statement binds in its enclosing scope, descending
/// into compound statements (which do not open a scope).
fn stmt_bound_names<'a>(stmt: &'a Stmt, out: &mut Vec<&'a str>, star: &mut bool) {
    match stmt {
        Stmt::Import(i) => {
            for a in &i.names {
                out.push(match &a.asname {
                    Some(n) => n.as_str(),
                    None => a.name.as_str().split('.').next().unwrap_or(""),
                });
            }
        }
        Stmt::ImportFrom(i) => {
            for a in &i.names {
                if a.name.as_str() == "*" {
                    *star = true;
                } else {
                    out.push(a.asname.as_ref().unwrap_or(&a.name).as_str());
                }
            }
        }
        Stmt::FunctionDef(f) => out.push(f.name.as_str()),
        Stmt::ClassDef(c) => out.push(c.name.as_str()),
        Stmt::Assign(a) => a.targets.iter().for_each(|t| target_names(t, out)),
        Stmt::AnnAssign(a) => target_names(&a.target, out),
        Stmt::AugAssign(a) => target_names(&a.target, out),
        Stmt::TypeAlias(t) => target_names(&t.name, out),
        Stmt::For(s) => {
            target_names(&s.target, out);
            body_names(&s.body, out, star);
            body_names(&s.orelse, out, star);
        }
        Stmt::While(s) => {
            body_names(&s.body, out, star);
            body_names(&s.orelse, out, star);
        }
        Stmt::If(s) => {
            body_names(&s.body, out, star);
            for clause in &s.elif_else_clauses {
                body_names(&clause.body, out, star);
            }
        }
        Stmt::With(s) => {
            for item in &s.items {
                if let Some(v) = &item.optional_vars {
                    target_names(v, out);
                }
            }
            body_names(&s.body, out, star);
        }
        Stmt::Try(s) => {
            body_names(&s.body, out, star);
            for h in &s.handlers {
                let ExceptHandler::ExceptHandler(h) = h;
                if let Some(n) = &h.name {
                    out.push(n.as_str());
                }
                body_names(&h.body, out, star);
            }
            body_names(&s.orelse, out, star);
            body_names(&s.finalbody, out, star);
        }
        Stmt::Match(s) => {
            for case in &s.cases {
                body_names(&case.body, out, star);
            }
        }
        _ => {}
    }
}

fn body_names<'a>(body: &'a [Stmt], out: &mut Vec<&'a str>, star: &mut bool) {
    for stmt in body {
        stmt_bound_names(stmt, out, star);
    }
}

fn target_names<'a>(target: &'a Expr, out: &mut Vec<&'a str>) {
    match target {
        Expr::Name(n) => out.push(n.id.as_str()),
        Expr::Tuple(t) => t.elts.iter().for_each(|e| target_names(e, out)),
        Expr::List(l) => l.elts.iter().for_each(|e| target_names(e, out)),
        Expr::Starred(s) => target_names(&s.value, out),
        _ => {}
    }
}

/// Names read while the `def` statement itself runs: decorators and
/// parameter defaults. Annotations are lazy in emitted Python (`from
/// __future__ import annotations`), and a lambda's body runs later.
fn def_time_names(
    decorators: &[Decorator],
    parameters: &ruff_python_ast::Parameters,
) -> Vec<(String, TextRange)> {
    struct Reads(Vec<(String, TextRange)>);
    impl<'b> Visitor<'b> for Reads {
        fn visit_expr(&mut self, e: &'b Expr) {
            match e {
                Expr::Name(n) => self.0.push((n.id.as_str().to_owned(), n.range())),
                Expr::Lambda(l) => {
                    if let Some(p) = &l.parameters {
                        for d in p
                            .iter_non_variadic_params()
                            .filter_map(|p| p.default.as_deref())
                        {
                            self.visit_expr(d);
                        }
                    }
                }
                _ => visitor::walk_expr(self, e),
            }
        }
    }
    let mut reads = Reads(Vec::new());
    for d in decorators {
        reads.visit_expr(&d.expression);
    }
    for p in parameters.iter_non_variadic_params() {
        if let Some(d) = p.default.as_deref() {
            reads.visit_expr(d);
        }
    }
    reads.0
}

/// Decorators whose effect is fixed when the class object is created, plus
/// the two the VM does not yet dispatch from a class attribute.
fn needs_class_creation(d: &Decorator) -> bool {
    fn last_segment(e: &Expr) -> Option<&str> {
        match e {
            Expr::Name(n) => Some(n.id.as_str()),
            Expr::Attribute(a) => Some(a.attr.as_str()),
            Expr::Call(c) => last_segment(&c.func),
            _ => None,
        }
    }
    last_segment(&d.expression).is_some_and(|n| {
        n.ends_with("cached_property")
            || n.starts_with("abstract")
            || matches!(n, "property" | "classmethod")
    })
}

/// Whether any identifier in `stmt` is subject to private-name mangling
/// (`__x`, not `__x__`), which only happens inside a class body.
fn uses_private_name(stmt: &Stmt) -> bool {
    fn private(name: &str) -> bool {
        name.starts_with("__") && !name.ends_with("__")
    }
    struct Finder(bool);
    impl<'b> Visitor<'b> for Finder {
        fn visit_stmt(&mut self, s: &'b Stmt) {
            match s {
                Stmt::FunctionDef(f) if private(f.name.as_str()) => self.0 = true,
                Stmt::ClassDef(c) if private(c.name.as_str()) => self.0 = true,
                Stmt::Global(g) if g.names.iter().any(|n| private(n.as_str())) => self.0 = true,
                Stmt::Nonlocal(g) if g.names.iter().any(|n| private(n.as_str())) => self.0 = true,
                _ => {}
            }
            visitor::walk_stmt(self, s);
        }
        fn visit_expr(&mut self, e: &'b Expr) {
            match e {
                Expr::Name(n) if private(n.id.as_str()) => self.0 = true,
                Expr::Attribute(a) if private(a.attr.as_str()) => self.0 = true,
                _ => {}
            }
            visitor::walk_expr(self, e);
        }
        fn visit_parameter(&mut self, p: &'b Parameter) {
            if private(p.name.as_str()) {
                self.0 = true;
            }
            visitor::walk_parameter(self, p);
        }
        fn visit_except_handler(&mut self, h: &'b ExceptHandler) {
            let ExceptHandler::ExceptHandler(inner) = h;
            if inner.name.as_ref().is_some_and(|n| private(n.as_str())) {
                self.0 = true;
            }
            visitor::walk_except_handler(self, h);
        }
        fn visit_alias(&mut self, a: &'b ruff_python_ast::Alias) {
            if a.asname.as_ref().is_some_and(|n| private(n.as_str())) {
                self.0 = true;
            }
        }
    }
    let mut finder = Finder(false);
    finder.visit_stmt(stmt);
    finder.0
}
