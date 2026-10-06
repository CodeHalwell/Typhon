//! Target-version gating: names the project's `[python] target` does not
//! have yet, and stdlib APIs it no longer ships.
//!
//! - `tyc::requires_python` — a builtin added after Python 3.13 (the oldest
//!   target) used on a target that predates it: `frozendict` (3.15, PEP 814)
//!   on a 3.13 project raises `NameError` when the emitted Python runs.
//! - `tyc::removed_in_python` — an import of a stdlib module or name the
//!   target removed (`sre_compile`, `typing.no_type_check_decorator`), or a
//!   call form it stopped accepting (`NamedTuple("P", x=int)`). The emitted
//!   Python raises `ImportError` / `AttributeError` / `TypeError` there.
//!
//! Both fire only on code that already fails on the target interpreter, so
//! neither narrows a program that runs correctly. The pass is deliberately
//! syntactic and conservative: a module that binds a gated name itself
//! (`from frozendict import frozendict` — the PyPI backport) is left alone,
//! and attribute access is only checked through a plain `import module`.

use std::collections::{HashMap, HashSet};

use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_ast::{self as ast, Expr, ModModule, Stmt};
use tyc_diagnostics::{Diagnostics, TycError};

/// Builtins added after Python 3.13: `(name, minor version, origin)`.
const NEW_BUILTINS: &[(&str, u8, &str)] =
    &[("frozendict", 15, "PEP 814"), ("sentinel", 15, "PEP 661")];

/// Stdlib modules added after Python 3.13: `(module, minor version, origin)`.
const NEW_MODULES: &[(&str, u8, &str)] = &[
    ("annotationlib", 14, "PEP 749"),
    ("compression", 14, "PEP 784"),
    ("concurrent.interpreters", 14, "PEP 734"),
    ("string.templatelib", 14, "PEP 750"),
    ("math.integer", 15, "PEP 791"),
    ("profiling", 15, "PEP 799"),
];

/// Stdlib names added after Python 3.13: `(module, name, minor version,
/// origin)` — `origin` names the PEP, when there is one.
const NEW_NAMES: &[(&str, &str, u8, &str)] = &[
    ("math", "fmax", 15, ""),
    ("math", "fmin", 15, ""),
    ("math", "integer", 15, "PEP 791"),
    ("math", "isnormal", 15, ""),
    ("math", "issubnormal", 15, ""),
    ("math", "signbit", 15, ""),
    ("re", "prefixmatch", 15, ""),
    ("sys", "get_lazy_imports", 15, "PEP 810"),
    ("sys", "set_lazy_imports", 15, "PEP 810"),
    ("threading", "concurrent_tee", 15, ""),
    ("threading", "serialize_iterator", 15, ""),
    ("threading", "synchronized_iterator", 15, ""),
    ("types", "FrameLocalsProxyType", 15, ""),
    ("types", "LazyImportType", 15, "PEP 810"),
    ("typing", "TypeForm", 15, "PEP 747"),
    ("typing", "disjoint_base", 15, "PEP 800"),
];

/// Stdlib modules removed after Python 3.13: `(module, removed in, advice)`.
const REMOVED_MODULES: &[(&str, u8, &str)] = &[
    (
        "sre_compile",
        15,
        "the `sre_*` modules were internal to `re`; use the `re` module",
    ),
    (
        "sre_constants",
        15,
        "the `sre_*` modules were internal to `re`; use the `re` module",
    ),
    (
        "sre_parse",
        15,
        "the `sre_*` modules were internal to `re`; use the `re` module",
    ),
];

/// Stdlib names removed after Python 3.13: `(module, name, removed in,
/// advice)`.
const REMOVED_NAMES: &[(&str, &str, u8, &str)] = &[
    ("ast", "Num", 14, "use `ast.Constant`"),
    ("ast", "Str", 14, "use `ast.Constant`"),
    ("ast", "Bytes", 14, "use `ast.Constant`"),
    ("ast", "NameConstant", 14, "use `ast.Constant`"),
    ("ast", "Ellipsis", 14, "use `ast.Constant`"),
    (
        "importlib.abc",
        "ResourceReader",
        14,
        "use `importlib.resources.abc.TraversableResources`",
    ),
    (
        "importlib.abc",
        "Traversable",
        14,
        "use `importlib.resources.abc.Traversable`",
    ),
    (
        "importlib.abc",
        "TraversableResources",
        14,
        "use `importlib.resources.abc.TraversableResources`",
    ),
    (
        "pkgutil",
        "find_loader",
        14,
        "use `importlib.util.find_spec`",
    ),
    ("pkgutil", "get_loader", 14, "use `importlib.util.find_spec`"),
    ("pty", "master_open", 14, "use `pty.openpty`"),
    ("pty", "slave_open", 14, "use `pty.openpty`"),
    (
        "sqlite3",
        "version",
        14,
        "it was the obsolete pysqlite version; `sqlite3.sqlite_version` is the SQLite library's",
    ),
    (
        "sqlite3",
        "version_info",
        14,
        "it was the obsolete pysqlite version; `sqlite3.sqlite_version_info` is the SQLite library's",
    ),
    (
        "urllib.request",
        "URLopener",
        14,
        "use `urllib.request.urlopen`",
    ),
    (
        "urllib.request",
        "FancyURLopener",
        14,
        "use `urllib.request.urlopen`",
    ),
    ("glob", "glob0", 15, "use `glob.glob(pattern, root_dir=…)`"),
    ("glob", "glob1", 15, "use `glob.glob(pattern, root_dir=…)`"),
    (
        "typing",
        "no_type_check_decorator",
        15,
        "drop it — no type checker ever honoured it",
    ),
];

/// Run both gates over `module`. `project_roots` are the top-level names of
/// the project's own modules: a project module that shares a new stdlib
/// module's name (`profiling.ty`) is the import's target, not the stdlib.
pub(super) fn check(
    module: &ModModule,
    python_minor: u8,
    path: &str,
    source: &str,
    project_roots: &HashSet<String>,
    diagnostics: &mut Diagnostics,
) {
    let mut bound = BoundNames::default();
    bound.visit_body(&module.body);
    let mut gate = Gate {
        python_minor,
        path,
        source,
        project_roots,
        bound: &bound.names,
        modules: HashMap::new(),
        typing_names: HashMap::new(),
        diagnostics,
    };
    gate.visit_body(&module.body);
}

fn target(minor: u8) -> String {
    format!("3.{minor}")
}

struct Gate<'a> {
    python_minor: u8,
    path: &'a str,
    source: &'a str,
    /// Top-level names of the project's own modules.
    project_roots: &'a HashSet<String>,
    /// Every name the module binds anywhere: a gated builtin bound here is
    /// the module's own (or an imported backport) and is not checked.
    bound: &'a HashSet<String>,
    /// Local name → module path, from `import m` / `import m as n` and
    /// `from pkg import sub` (`abc` → `importlib.abc`).
    modules: HashMap<String, String>,
    /// Local name → `typing` member, for the call-form checks
    /// (`from typing import NamedTuple as NT` → `NT` → `NamedTuple`).
    typing_names: HashMap<String, String>,
    diagnostics: &'a mut Diagnostics,
}

impl Gate<'_> {
    fn requires(&mut self, name: &str, since: u8, origin: &str, range: ruff_text_size::TextRange) {
        self.requires_with(name, since, origin, "NameError", range);
    }

    fn requires_with(
        &mut self,
        name: &str,
        since: u8,
        origin: &str,
        error: &str,
        range: ruff_text_size::TextRange,
    ) {
        let origin = if origin.is_empty() {
            String::new()
        } else {
            format!(" ({origin})")
        };
        let message = format!(
            "`{name}` is new in Python {}{origin}, but this project targets Python {}",
            target(since),
            target(self.python_minor),
        );
        let help = format!(
            "set `[python] target = \"{}\"` (or newer) in typhon.toml — the emitted Python would \
             raise `{error}` on {}",
            target(since),
            target(self.python_minor),
        );
        self.diagnostics.push_error(TycError::requires_python(
            message,
            help,
            self.path,
            self.source,
            range.start().to_usize(),
            range.len().to_usize(),
        ));
    }

    fn removed(&mut self, what: String, since: u8, advice: &str, range: ruff_text_size::TextRange) {
        let message = format!(
            "{what} was removed in Python {}, which this project targets ({})",
            target(since),
            target(self.python_minor),
        );
        self.diagnostics.push_error(TycError::removed_in_python(
            message,
            advice.to_owned(),
            self.path,
            self.source,
            range.start().to_usize(),
            range.len().to_usize(),
        ));
    }

    /// A stdlib module newer than the target that `module` names (itself or
    /// a submodule of it), unless it is the project's own module.
    fn new_module(&self, module: &str) -> Option<(&'static str, u8, &'static str)> {
        let root = module.split('.').next().unwrap_or(module);
        if self.project_roots.contains(root) {
            return None;
        }
        NEW_MODULES
            .iter()
            .find(|(m, since, _)| {
                (module == *m || module.starts_with(&format!("{m}."))) && self.python_minor < *since
            })
            .copied()
    }

    /// A stdlib name newer than the target.
    fn new_name(&self, module: &str, name: &str) -> Option<(u8, &'static str)> {
        if self
            .project_roots
            .contains(module.split('.').next().unwrap_or(module))
        {
            return None;
        }
        NEW_NAMES
            .iter()
            .find(|(m, n, since, _)| *m == module && *n == name && self.python_minor < *since)
            .map(|(_, _, since, origin)| (*since, *origin))
    }

    fn removed_module(&self, module: &str) -> Option<(u8, &'static str)> {
        REMOVED_MODULES
            .iter()
            .find(|(m, since, _)| {
                (module == *m || module.starts_with(&format!("{m}.")))
                    && self.python_minor >= *since
            })
            .map(|(_, since, advice)| (*since, *advice))
    }

    fn removed_name(&self, module: &str, name: &str) -> Option<(u8, &'static str)> {
        REMOVED_NAMES
            .iter()
            .find(|(m, n, since, _)| *m == module && *n == name && self.python_minor >= *since)
            .map(|(_, _, since, advice)| (*since, *advice))
    }

    /// The module path `expr` names through the tracked imports:
    /// `urllib.request` for `urllib.request` after `import urllib.request`,
    /// `importlib.abc` for `abc` after `from importlib import abc`.
    fn module_path(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Name(n) => self.modules.get(n.id.as_str()).cloned(),
            Expr::Attribute(a) => {
                let base = self.module_path(&a.value)?;
                Some(format!("{base}.{}", a.attr.as_str()))
            }
            _ => None,
        }
    }

    /// The `typing` member a call's callee names, if any.
    fn typing_callee(&self, func: &Expr) -> Option<String> {
        match func {
            Expr::Name(n) => self.typing_names.get(n.id.as_str()).cloned(),
            Expr::Attribute(a) => (self.module_path(&a.value).as_deref() == Some("typing"))
                .then(|| a.attr.as_str().to_owned()),
            _ => None,
        }
    }

    /// Functional `NamedTuple` / `TypedDict` forms Python 3.15 rejects with
    /// `TypeError` (3.13 and 3.14 only warned).
    fn check_typing_call(&mut self, call: &ast::ExprCall) {
        if self.python_minor < 15 {
            return;
        }
        let Some(member) = self.typing_callee(&call.func) else {
            return;
        };
        let args = &call.arguments;
        if args.args.iter().any(|a| matches!(a, Expr::Starred(_)))
            || args.keywords.iter().any(|k| k.arg.is_none())
        {
            return;
        }
        let none_fields = matches!(args.args.get(1), Some(Expr::NoneLiteral(_)));
        let problem = match member.as_str() {
            "NamedTuple" if !args.keywords.is_empty() => {
                Some("`NamedTuple(\"Name\", field=type)` (keyword fields)")
            }
            "NamedTuple" | "TypedDict" if args.args.len() == 1 && args.keywords.is_empty() => {
                Some("a functional `NamedTuple` / `TypedDict` with no fields argument")
            }
            "NamedTuple" | "TypedDict" if none_fields => {
                Some("a functional `NamedTuple` / `TypedDict` with `None` as its fields")
            }
            _ => None,
        };
        if let Some(what) = problem {
            let advice = if member == "NamedTuple" {
                "pass the fields as a list — `NamedTuple(\"P\", [(\"x\", int)])` — or use a class"
            } else {
                "pass a dict of fields — `TypedDict(\"T\", {})` for none — or use a class"
            };
            self.removed(what.to_owned(), 15, advice, call.range);
        }
    }
}

impl<'ast> Visitor<'ast> for Gate<'_> {
    fn visit_stmt(&mut self, stmt: &'ast Stmt) {
        match stmt {
            Stmt::Import(i) => {
                for alias in &i.names {
                    let module = alias.name.as_str();
                    if let Some((since, advice)) = self.removed_module(module) {
                        self.removed(format!("the `{module}` module"), since, advice, alias.range);
                    }
                    if let Some((new, since, origin)) = self.new_module(module) {
                        self.requires_with(new, since, origin, "ModuleNotFoundError", alias.range);
                    }
                    match &alias.asname {
                        Some(asname) => {
                            self.modules.insert(asname.to_string(), module.to_owned());
                        }
                        None => {
                            // `import a.b` binds `a`; `a.b.x` is reached
                            // through attribute access on it.
                            let root = module.split('.').next().unwrap_or(module);
                            self.modules.insert(root.to_owned(), root.to_owned());
                        }
                    }
                }
            }
            Stmt::ImportFrom(f) if f.level == 0 => {
                let module = f.module.as_ref().map(|m| m.as_str()).unwrap_or("");
                if let Some((since, advice)) = self.removed_module(module) {
                    self.removed(format!("the `{module}` module"), since, advice, f.range);
                } else if let Some((new, since, origin)) = self.new_module(module) {
                    self.requires_with(new, since, origin, "ModuleNotFoundError", f.range);
                } else {
                    for alias in &f.names {
                        let name = alias.name.as_str();
                        if let Some((since, advice)) = self.removed_name(module, name) {
                            self.removed(format!("`{module}.{name}`"), since, advice, alias.range);
                        }
                        if let Some((since, origin)) = self.new_name(module, name) {
                            let qualified = format!("{module}.{name}");
                            self.requires_with(
                                &qualified,
                                since,
                                origin,
                                "ImportError",
                                alias.range,
                            );
                        }
                        let local = alias.asname.as_ref().unwrap_or(&alias.name).to_string();
                        if module == "typing" {
                            self.typing_names.insert(local.clone(), name.to_owned());
                        }
                        self.modules.insert(local, format!("{module}.{name}"));
                    }
                }
            }
            _ => {}
        }
        visitor::walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'ast Expr) {
        match expr {
            Expr::Name(n) if matches!(n.ctx, ast::ExprContext::Load) => {
                let name = n.id.as_str();
                if let Some((_, since, origin)) =
                    NEW_BUILTINS.iter().find(|(builtin, _, _)| *builtin == name)
                {
                    if self.python_minor < *since && !self.bound.contains(name) {
                        self.requires(name, *since, origin, n.range);
                    }
                }
            }
            Expr::Attribute(a) => {
                if let Some(module) = self.module_path(&a.value) {
                    let attr = a.attr.as_str();
                    if let Some((since, advice)) = self.removed_name(&module, attr) {
                        self.removed(format!("`{module}.{attr}`"), since, advice, a.range);
                    }
                    if let Some((since, origin)) = self.new_name(&module, attr) {
                        let qualified = format!("{module}.{attr}");
                        self.requires_with(&qualified, since, origin, "AttributeError", a.range);
                    }
                }
            }
            Expr::Call(call) => self.check_typing_call(call),
            _ => {}
        }
        visitor::walk_expr(self, expr);
    }
}

/// Every name a module binds, in any scope.
#[derive(Default)]
struct BoundNames {
    names: HashSet<String>,
}

impl BoundNames {
    fn add(&mut self, name: &str) {
        self.names.insert(name.to_owned());
    }
}

impl<'ast> Visitor<'ast> for BoundNames {
    fn visit_stmt(&mut self, stmt: &'ast Stmt) {
        match stmt {
            Stmt::FunctionDef(f) => self.add(f.name.as_str()),
            Stmt::ClassDef(c) => self.add(c.name.as_str()),
            Stmt::Import(i) => {
                for alias in &i.names {
                    match &alias.asname {
                        Some(asname) => self.add(asname.as_str()),
                        None => self.add(alias.name.split('.').next().unwrap_or("")),
                    }
                }
            }
            Stmt::ImportFrom(f) => {
                for alias in &f.names {
                    self.add(alias.asname.as_ref().unwrap_or(&alias.name).as_str());
                }
            }
            Stmt::Global(g) => g.names.iter().for_each(|n| self.add(n.as_str())),
            Stmt::Nonlocal(n) => n.names.iter().for_each(|n| self.add(n.as_str())),
            Stmt::TypeAlias(t) => {
                if let Expr::Name(n) = t.name.as_ref() {
                    self.add(n.id.as_str());
                }
            }
            _ => {}
        }
        visitor::walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'ast Expr) {
        if let Expr::Name(n) = expr {
            if !matches!(n.ctx, ast::ExprContext::Load) {
                self.add(n.id.as_str());
            }
        }
        visitor::walk_expr(self, expr);
    }

    fn visit_parameter(&mut self, parameter: &'ast ast::Parameter) {
        self.add(parameter.name.as_str());
        visitor::walk_parameter(self, parameter);
    }

    fn visit_except_handler(&mut self, handler: &'ast ast::ExceptHandler) {
        let ast::ExceptHandler::ExceptHandler(h) = handler;
        if let Some(name) = &h.name {
            self.add(name.as_str());
        }
        visitor::walk_except_handler(self, handler);
    }

    fn visit_pattern(&mut self, pattern: &'ast ast::Pattern) {
        match pattern {
            ast::Pattern::MatchAs(p) => {
                if let Some(name) = &p.name {
                    self.add(name.as_str());
                }
            }
            ast::Pattern::MatchStar(p) => {
                if let Some(name) = &p.name {
                    self.add(name.as_str());
                }
            }
            ast::Pattern::MatchMapping(p) => {
                if let Some(rest) = &p.rest {
                    self.add(rest.as_str());
                }
            }
            _ => {}
        }
        visitor::walk_pattern(self, pattern);
    }

    fn visit_type_param(&mut self, type_param: &'ast ast::TypeParam) {
        self.add(type_param.name().as_str());
        visitor::walk_type_param(self, type_param);
    }
}
