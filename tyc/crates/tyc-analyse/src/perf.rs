//! Performance-advice lints — the `tyc::perf_*` family plus
//! `tyc::lazy_import_opportunity`.
//!
//! Every lint here is **advice** severity: it never blocks a build and
//! never changes what programs are accepted. The whole family is gated by
//! `[strictness] suggest-perf` (default `true`) via
//! [`crate::LintOptions::suggest_perf`]. They surface in `tyc check` /
//! `tyc build` and, through the shared [`crate::editor_lint_diagnostics`],
//! as live LSP hints — mirroring `tyc::gather_opportunity`.
//!
//! **Conservatism is the prime directive.** Each lint fires only when the
//! pattern is unambiguous from local AST evidence: a binding annotated
//! `list[...]` / `str` / `dict[...]` (Typhon requires the annotation, so
//! the receiver type is on the node), a loop-invariance proof from a
//! same-body mutation scan, and so on. When in doubt, they stay silent —
//! the example/stress corpus must remain advice-noise-free.
//!
//! The seven lints:
//!
//! 1. `tyc::perf_membership_in_loop` — `x in LIST` inside a loop whose
//!    `LIST` is never mutated in that loop → build a `set` once outside.
//! 2. `tyc::perf_list_shift_in_loop` — `LIST.insert(0, …)` / `LIST.pop(0)`
//!    inside a loop → `collections.deque`.
//! 3. `tyc::perf_str_concat_in_loop` — `s += …` on a `str` inside a loop
//!    → collect parts in a `list[str]` and `"".join(...)`.
//! 4. `tyc::perf_sort_in_loop` — `sorted(x)` / `x.sort()` inside a loop
//!    whose `x` is loop-invariant → sort once, or `heapq`.
//! 5. `tyc::perf_sorted_first` — `sorted(...)[0]` / `sorted(...)[-1]`
//!    → `min(...)` / `max(...)`.
//! 6. `tyc::perf_keys_membership` — `k in d.keys()` → `k in d`.
//! 7. `tyc::lazy_import_opportunity` — a module-level `import X` used only
//!    inside function bodies → `lazy import`.

use std::collections::{HashMap, HashSet};

use ruff_python_ast::{
    visitor::source_order::{walk_expr, SourceOrderVisitor},
    CmpOp, Expr, ExprContext, ModModule, Number, Operator, Parameters, Stmt, UnaryOp,
};
use ruff_text_size::{Ranged, TextRange};
use tyc_diagnostics::{Diagnostics, TycError};

// ── stdlib top-level module table (shared with `tyc::stdlib_module_shadow`) ────

/// True when `name` is the top-level name of a Python 3.13 stdlib module.
///
/// This is the single source of truth behind both `tyc::stdlib_module_shadow`
/// (`tyc/crates/tyc/src/commands/check.rs` delegates here) and
/// `tyc::lazy_import_opportunity` (which skips stdlib imports — deferring a
/// stdlib import buys nothing and risks shadowing an already-loaded module).
/// Keeping the table in one place avoids the two consumers drifting apart.
pub fn is_stdlib_top_level(name: &str) -> bool {
    STDLIB_TOP_LEVEL.contains(&name)
}

/// The curated set of Python 3.13 stdlib top-level module names. Restricted
/// to top-level modules (subpackages like `urllib.parse` are excluded).
const STDLIB_TOP_LEVEL: &[&str] = &[
    "abc",
    "argparse",
    "array",
    "ast",
    "asyncio",
    "atexit",
    "audioop",
    "base64",
    "bdb",
    "bisect",
    "builtins",
    "bz2",
    "calendar",
    "cmath",
    "cmd",
    "code",
    "codecs",
    "codeop",
    "collections",
    "colorsys",
    "compileall",
    "concurrent",
    "configparser",
    "contextlib",
    "contextvars",
    "copy",
    "copyreg",
    "cProfile",
    "csv",
    "ctypes",
    "curses",
    "dataclasses",
    "datetime",
    "dbm",
    "decimal",
    "difflib",
    "dis",
    "doctest",
    "email",
    "encodings",
    "enum",
    "errno",
    "faulthandler",
    "fcntl",
    "filecmp",
    "fileinput",
    "fnmatch",
    "fractions",
    "ftplib",
    "functools",
    "gc",
    "getopt",
    "getpass",
    "gettext",
    "glob",
    "graphlib",
    "gzip",
    "hashlib",
    "heapq",
    "hmac",
    "html",
    "http",
    "imaplib",
    "importlib",
    "inspect",
    "io",
    "ipaddress",
    "itertools",
    "json",
    "keyword",
    "linecache",
    "locale",
    "logging",
    "lzma",
    "mailbox",
    "marshal",
    "math",
    "mimetypes",
    "mmap",
    "modulefinder",
    "multiprocessing",
    "netrc",
    "numbers",
    "operator",
    "optparse",
    "os",
    "parser",
    "pathlib",
    "pdb",
    "pickle",
    "pickletools",
    "pkgutil",
    "platform",
    "plistlib",
    "poplib",
    "posix",
    "posixpath",
    "pprint",
    "profile",
    "pstats",
    "pty",
    "pwd",
    "py_compile",
    "pyclbr",
    "pydoc",
    "queue",
    "quopri",
    "random",
    "re",
    "readline",
    "reprlib",
    "resource",
    "rlcompleter",
    "runpy",
    "sched",
    "secrets",
    "select",
    "selectors",
    "shelve",
    "shlex",
    "shutil",
    "signal",
    "site",
    "smtplib",
    "socket",
    "socketserver",
    "sqlite3",
    "ssl",
    "stat",
    "statistics",
    "string",
    "stringprep",
    "struct",
    "subprocess",
    "symtable",
    "sys",
    "sysconfig",
    "syslog",
    "tabnanny",
    "tarfile",
    "telnetlib",
    "tempfile",
    "termios",
    "test",
    "textwrap",
    "this",
    "threading",
    "time",
    "timeit",
    "tkinter",
    "token",
    "tokenize",
    "tomllib",
    "trace",
    "traceback",
    "tracemalloc",
    "tty",
    "turtle",
    "types",
    "typing",
    "unicodedata",
    "unittest",
    "urllib",
    "uu",
    "uuid",
    "venv",
    "warnings",
    "wave",
    "weakref",
    "webbrowser",
    "winreg",
    "winsound",
    "wsgiref",
    "xdrlib",
    "xml",
    "xmlrpc",
    "zipapp",
    "zipfile",
    "zipimport",
    "zlib",
    "zoneinfo",
];

// ── preprocess-derived context (facts stripped before the AST) ────────────────

/// Facts the perf lints need that don't survive into the parsed AST. The
/// preprocessor strips `pub` / `lazy` markers and rewrites `lazy import
/// ALIAS = MODULE` to a plain `import MODULE as ALIAS`, so
/// `lazy_import_opportunity` can't recover them from the module alone.
///
/// Callers build this from the [`tyc_syntax::preprocess::PreprocessResult`]
/// (`tyc check` / `tyc build`) or the cached `preprocessed_full` Salsa query
/// (the LSP). [`Default`] (everything empty / `false`) is the safe fallback
/// for a standalone buffer.
#[derive(Debug, Default, Clone)]
pub struct PerfLintContext {
    /// Local aliases of imports that are already `lazy import`s. They
    /// preprocess to `import MODULE as ALIAS`, so the raw AST can't tell
    /// them apart from a plain aliased import — without this list
    /// `lazy_import_opportunity` would suggest making an already-lazy
    /// import lazy.
    pub lazy_import_aliases: Vec<String>,
    /// Module-level names marked `pub` (the marker is stripped by
    /// preprocess). A `pub`-exported name is part of the module's public
    /// surface, so it's exempt from the lazy-import nudge.
    pub pub_names: Vec<String>,
    /// The file carries a `pub *` wildcard re-export (only meaningful in
    /// `__init__.ty`). Exempts every import in the file from the
    /// lazy-import nudge.
    pub has_pub_star: bool,
}

// ── public entry point ────────────────────────────────────────────────────────

/// Run the whole `tyc::perf_*` advice family over `module` and collect the
/// diagnostics. `source` must be the *preprocessed* Python the `module` was
/// parsed from (spans are byte offsets into it); `ctx` carries the
/// preprocess-derived facts `lazy_import_opportunity` needs.
///
/// The single entry point mirrors [`crate::gather_opportunity_diagnostics`]:
/// `tyc check` (via [`crate::editor_lint_diagnostics`]), `tyc build`, and the
/// LSP all route through it so the three surfaces never drift.
pub fn perf_diagnostics(
    module: &ModModule,
    path: &str,
    source: &str,
    ctx: &PerfLintContext,
) -> Diagnostics {
    let mut diags = Diagnostics::new();
    // Loop-scoped lints (membership / list-shift / str-concat / sort).
    let module_env = collect_scope_bindings(&module.body, &HashMap::new(), None);
    walk_scope(
        &module.body,
        &module_env,
        /* loop_mutated */ None,
        path,
        source,
        &mut diags,
    );
    // Expression-shape lints that are safe anywhere (not loop-scoped).
    {
        let mut anywhere = AnywhereVisitor {
            path,
            source,
            diags: &mut diags,
        };
        for stmt in &module.body {
            anywhere.visit_stmt(stmt);
        }
    }
    // Startup-cost lint.
    lazy_import_opportunity_diagnostics(module, path, source, ctx, &mut diags);
    diags
}

// ── annotation classification + scope environment ─────────────────────────────

/// The receiver kinds the perf lints care about. A binding's kind comes
/// from its annotation (Typhon requires one) or, for lists, a list-literal
/// initialiser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnnKind {
    List,
    Str,
    Dict,
    Set,
}

/// Classify an annotation expression. Only the bare head and the
/// single-subscript forms are recognised (`list`, `list[int]`, `str`,
/// `dict`, `dict[str, int]`, `set`, `frozenset[int]`); a nullable
/// `list[int] | None` (a `BinOp`) is deliberately *not* a list — the value
/// might be `None`, so narrowing would be required and we stay silent.
fn classify_annotation(expr: &Expr) -> Option<AnnKind> {
    let head = match expr {
        Expr::Name(n) => n.id.as_str(),
        Expr::Subscript(s) => match s.value.as_ref() {
            Expr::Name(n) => n.id.as_str(),
            _ => return None,
        },
        _ => return None,
    };
    match head {
        "list" => Some(AnnKind::List),
        "str" => Some(AnnKind::Str),
        "dict" => Some(AnnKind::Dict),
        "set" | "frozenset" => Some(AnnKind::Set),
        _ => None,
    }
}

/// Build the name → [`AnnKind`] environment for a lexical scope: `base`
/// (an enclosing scope's env), plus this scope's parameters (if any) and
/// every `NAME: TYPE = …` / `NAME = [list literal]` binding in `body`.
///
/// Recurses into control-flow blocks (they share the scope) but **not** into
/// nested `def` / `class` bodies — those introduce their own scopes and are
/// visited with a freshly-built env by [`walk_scope`].
fn collect_scope_bindings(
    body: &[Stmt],
    base: &HashMap<String, AnnKind>,
    params: Option<&Parameters>,
) -> HashMap<String, AnnKind> {
    let mut env = base.clone();
    if let Some(p) = params {
        for a in p
            .posonlyargs
            .iter()
            .chain(p.args.iter())
            .chain(p.kwonlyargs.iter())
        {
            if let Some(ann) = a.parameter.annotation.as_deref() {
                if let Some(k) = classify_annotation(ann) {
                    env.insert(a.parameter.name.to_string(), k);
                }
            }
        }
    }
    collect_bindings_into(body, &mut env);
    env
}

fn collect_bindings_into(body: &[Stmt], env: &mut HashMap<String, AnnKind>) {
    for stmt in body {
        match stmt {
            Stmt::AnnAssign(a) => {
                if let Expr::Name(n) = a.target.as_ref() {
                    if let Some(k) = classify_annotation(&a.annotation) {
                        env.insert(n.id.to_string(), k);
                    }
                }
            }
            Stmt::Assign(a) => {
                if a.targets.len() == 1 {
                    if let (Expr::Name(n), Expr::List(_)) = (&a.targets[0], a.value.as_ref()) {
                        env.insert(n.id.to_string(), AnnKind::List);
                    }
                }
            }
            // Control-flow blocks share the scope; nested def/class do not.
            Stmt::If(s) => {
                collect_bindings_into(&s.body, env);
                for c in &s.elif_else_clauses {
                    collect_bindings_into(&c.body, env);
                }
            }
            Stmt::While(s) => {
                collect_bindings_into(&s.body, env);
                collect_bindings_into(&s.orelse, env);
            }
            Stmt::For(s) => {
                collect_bindings_into(&s.body, env);
                collect_bindings_into(&s.orelse, env);
            }
            Stmt::With(s) => collect_bindings_into(&s.body, env),
            Stmt::Try(s) => {
                collect_bindings_into(&s.body, env);
                collect_bindings_into(&s.orelse, env);
                collect_bindings_into(&s.finalbody, env);
                for h in &s.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    collect_bindings_into(&h.body, env);
                }
            }
            Stmt::Match(s) => {
                for case in &s.cases {
                    collect_bindings_into(&case.body, env);
                }
            }
            _ => {}
        }
    }
}

// ── loop-scoped walker (lints 1–4) ────────────────────────────────────────────

/// Mutations that change a collection's *element set* — the ones that break
/// loop-invariance for lints 1 (membership) and 4 (sort). Reordering
/// operations (`sort` / `reverse`) are deliberately excluded: they change
/// neither membership results nor the output of a later `sorted(...)`, so a
/// collection that is only reordered in the loop is still "invariant" for
/// these lints (indeed a repeated `.sort()` of unchanged data is exactly
/// what lint 4 flags). Rebinding / subscript-store / `del` are caught
/// structurally by [`names_mutated`], independent of this table.
const CONTENT_MUTATORS: &[&str] = &[
    "append",
    "extend",
    "insert",
    "remove",
    "pop",
    "clear",
    "add",
    "discard",
    "update",
    "appendleft",
    "popleft",
    "extendleft",
    "rotate",
    "popitem",
    "setdefault",
];

/// Walk a lexical scope, tracking the innermost enclosing loop's
/// content-mutation set (`None` when not inside a loop). Runs the
/// loop-scoped lints (1–4) on each in-loop statement and descends into new
/// scopes with a freshly-built environment.
fn walk_scope(
    stmts: &[Stmt],
    env: &HashMap<String, AnnKind>,
    loop_mutated: Option<&HashSet<String>>,
    path: &str,
    source: &str,
    diags: &mut Diagnostics,
) {
    for stmt in stmts {
        match stmt {
            Stmt::FunctionDef(f) => {
                let fenv = collect_scope_bindings(&f.body, env, Some(&f.parameters));
                walk_scope(&f.body, &fenv, None, path, source, diags);
            }
            Stmt::ClassDef(c) => {
                // Methods are `FunctionDef`s handled above; the class body
                // itself isn't a loop scope.
                walk_scope(&c.body, env, None, path, source, diags);
            }
            Stmt::For(s) => {
                if loop_mutated.is_some() {
                    check_stmt_in_loop(stmt, env, loop_mutated, path, source, diags);
                }
                // The loop target is re-bound every iteration, so it is NOT
                // loop-invariant — fold its names into the mutated set so a
                // `sorted(target)` / `target in list` in the body doesn't
                // read as invariant.
                let mut mutated = names_mutated(&s.body, CONTENT_MUTATORS);
                collect_target_names(&s.target, &mut mutated);
                walk_scope(&s.body, env, Some(&mutated), path, source, diags);
                walk_scope(&s.orelse, env, loop_mutated, path, source, diags);
            }
            Stmt::While(s) => {
                if loop_mutated.is_some() {
                    check_stmt_in_loop(stmt, env, loop_mutated, path, source, diags);
                }
                let mutated = names_mutated(&s.body, CONTENT_MUTATORS);
                walk_scope(&s.body, env, Some(&mutated), path, source, diags);
                walk_scope(&s.orelse, env, loop_mutated, path, source, diags);
            }
            Stmt::If(s) => {
                if loop_mutated.is_some() {
                    check_stmt_in_loop(stmt, env, loop_mutated, path, source, diags);
                }
                walk_scope(&s.body, env, loop_mutated, path, source, diags);
                for c in &s.elif_else_clauses {
                    walk_scope(&c.body, env, loop_mutated, path, source, diags);
                }
            }
            Stmt::With(s) => {
                if loop_mutated.is_some() {
                    check_stmt_in_loop(stmt, env, loop_mutated, path, source, diags);
                }
                walk_scope(&s.body, env, loop_mutated, path, source, diags);
            }
            Stmt::Try(s) => {
                if loop_mutated.is_some() {
                    check_stmt_in_loop(stmt, env, loop_mutated, path, source, diags);
                }
                walk_scope(&s.body, env, loop_mutated, path, source, diags);
                walk_scope(&s.orelse, env, loop_mutated, path, source, diags);
                walk_scope(&s.finalbody, env, loop_mutated, path, source, diags);
                for h in &s.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    walk_scope(&h.body, env, loop_mutated, path, source, diags);
                }
            }
            Stmt::Match(s) => {
                if loop_mutated.is_some() {
                    check_stmt_in_loop(stmt, env, loop_mutated, path, source, diags);
                }
                for case in &s.cases {
                    walk_scope(&case.body, env, loop_mutated, path, source, diags);
                }
            }
            _ => {
                if loop_mutated.is_some() {
                    check_stmt_in_loop(stmt, env, loop_mutated, path, source, diags);
                }
            }
        }
    }
}

/// Run the loop-scoped lints on a single in-loop statement's *own*
/// expressions (never nested statement bodies — the walker recurses into
/// those itself). Lint 1 examines `if`/`while` conditions; lints 2 & 4 scan
/// header expressions for calls; lint 3 matches an augmented string assign.
fn check_stmt_in_loop(
    stmt: &Stmt,
    env: &HashMap<String, AnnKind>,
    loop_mutated: Option<&HashSet<String>>,
    path: &str,
    source: &str,
    diags: &mut Diagnostics,
) {
    // Lint 3 — `NAME += EXPR` on a `str` binding, plain-name target only.
    if let Stmt::AugAssign(a) = stmt {
        if matches!(a.op, Operator::Add) {
            if let Expr::Name(n) = a.target.as_ref() {
                if matches!(env.get(n.id.as_str()), Some(AnnKind::Str)) {
                    let range = a.target.range();
                    diags.push_warning(TycError::perf_str_concat_in_loop(
                        n.id.as_str(),
                        path,
                        source,
                        range.start().to_usize(),
                        span_len(range),
                    ));
                }
            }
        }
    }

    // Lint 1 — membership tests in `if`/`while` conditions.
    match stmt {
        Stmt::If(s) => {
            scan_membership_in_test(&s.test, env, loop_mutated, path, source, diags);
            for c in &s.elif_else_clauses {
                if let Some(test) = &c.test {
                    scan_membership_in_test(test, env, loop_mutated, path, source, diags);
                }
            }
        }
        Stmt::While(s) => scan_membership_in_test(&s.test, env, loop_mutated, path, source, diags),
        _ => {}
    }

    // Lints 2 & 4 — call shapes in the statement's own (header) expressions.
    for expr in stmt_header_exprs(stmt) {
        let mut v = CallLintVisitor {
            env,
            loop_mutated,
            path,
            source,
            diags,
        };
        v.visit_expr(expr);
    }
}

/// The expressions a statement evaluates directly, excluding any nested
/// statement bodies. `for`/`while`/`def`/`class` headers are intentionally
/// omitted (a loop header runs once per enclosing iteration but flagging
/// `for x in sorted(xs):` is noisy; new scopes are visited separately).
fn stmt_header_exprs(stmt: &Stmt) -> Vec<&Expr> {
    match stmt {
        Stmt::If(s) => vec![&s.test],
        Stmt::With(s) => s.items.iter().map(|i| &i.context_expr).collect(),
        Stmt::Match(s) => vec![&s.subject],
        Stmt::Assign(s) => vec![&s.value],
        Stmt::AnnAssign(s) => s.value.as_deref().into_iter().collect(),
        Stmt::AugAssign(s) => vec![&s.value],
        Stmt::Return(s) => s.value.as_deref().into_iter().collect(),
        Stmt::Expr(s) => vec![&s.value],
        Stmt::Assert(s) => {
            let mut v = vec![s.test.as_ref()];
            if let Some(m) = s.msg.as_deref() {
                v.push(m);
            }
            v
        }
        // `While`, `For`, `FunctionDef`, `ClassDef`: headers not scanned.
        _ => Vec::new(),
    }
}

/// Find `X in NAME` / `X not in NAME` membership tests in a condition
/// expression (recursing through boolean operators) where `NAME` is a
/// `list` binding that the enclosing loop never content-mutates.
fn scan_membership_in_test(
    test: &Expr,
    env: &HashMap<String, AnnKind>,
    loop_mutated: Option<&HashSet<String>>,
    path: &str,
    source: &str,
    diags: &mut Diagnostics,
) {
    struct V<'a> {
        env: &'a HashMap<String, AnnKind>,
        loop_mutated: Option<&'a HashSet<String>>,
        path: &'a str,
        source: &'a str,
        diags: &'a mut Diagnostics,
    }
    impl<'ast, 'a> SourceOrderVisitor<'ast> for V<'a> {
        fn visit_expr(&mut self, e: &'ast Expr) {
            if let Expr::Compare(c) = e {
                if c.ops.len() == 1 && matches!(c.ops[0], CmpOp::In | CmpOp::NotIn) {
                    if let Some(Expr::Name(rhs)) = c.comparators.first() {
                        let name = rhs.id.as_str();
                        let is_list = matches!(self.env.get(name), Some(AnnKind::List));
                        let invariant = self.loop_mutated.is_none_or(|m| !m.contains(name));
                        if is_list && invariant {
                            let range = c.range();
                            self.diags.push_warning(TycError::perf_membership_in_loop(
                                name,
                                self.path,
                                self.source,
                                range.start().to_usize(),
                                span_len(range),
                            ));
                        }
                    }
                }
            }
            walk_expr(self, e);
        }
    }
    let mut v = V {
        env,
        loop_mutated,
        path,
        source,
        diags,
    };
    v.visit_expr(test);
}

/// Visitor for the call-shaped in-loop lints (2 & 4). Applied to a
/// statement's own expressions.
struct CallLintVisitor<'a> {
    env: &'a HashMap<String, AnnKind>,
    loop_mutated: Option<&'a HashSet<String>>,
    path: &'a str,
    source: &'a str,
    diags: &'a mut Diagnostics,
}

impl<'ast, 'a> SourceOrderVisitor<'ast> for CallLintVisitor<'a> {
    fn visit_expr(&mut self, e: &'ast Expr) {
        if let Expr::Call(call) = e {
            self.check_call(call);
        }
        walk_expr(self, e);
    }
}

impl CallLintVisitor<'_> {
    fn invariant(&self, name: &str) -> bool {
        self.loop_mutated.is_none_or(|m| !m.contains(name))
    }

    fn check_call(&mut self, call: &ruff_python_ast::ExprCall) {
        match call.func.as_ref() {
            // `NAME.method(...)` — list-shift (lint 2) or `.sort()` (lint 4).
            Expr::Attribute(attr) => {
                let Expr::Name(recv) = attr.value.as_ref() else {
                    return;
                };
                let name = recv.id.as_str();
                let method = attr.attr.as_str();
                let is_list = matches!(self.env.get(name), Some(AnnKind::List));
                // Lint 2 — `LIST.insert(0, …)` / `LIST.pop(0)`.
                if is_list {
                    let op = if method == "insert"
                        && call.arguments.args.first().is_some_and(is_int_zero_literal)
                    {
                        Some("insert(0, …)")
                    } else if method == "pop"
                        && call.arguments.args.len() == 1
                        && call.arguments.keywords.is_empty()
                        && is_int_zero_literal(&call.arguments.args[0])
                    {
                        Some("pop(0)")
                    } else {
                        None
                    };
                    if let Some(op) = op {
                        let range = call.range();
                        self.diags.push_warning(TycError::perf_list_shift_in_loop(
                            op,
                            self.path,
                            self.source,
                            range.start().to_usize(),
                            span_len(range),
                        ));
                    }
                }
                // Lint 4 — `NAME.sort(...)` on a loop-invariant receiver.
                if method == "sort" && self.invariant(name) {
                    let range = call.range();
                    self.diags.push_warning(TycError::perf_sort_in_loop(
                        self.path,
                        self.source,
                        range.start().to_usize(),
                        span_len(range),
                    ));
                }
            }
            // `sorted(NAME, ...)` — lint 4 with a loop-invariant argument.
            Expr::Name(f) if f.id.as_str() == "sorted" => {
                if let Some(Expr::Name(arg)) = call.arguments.args.first() {
                    if self.invariant(arg.id.as_str()) {
                        let range = call.range();
                        self.diags.push_warning(TycError::perf_sort_in_loop(
                            self.path,
                            self.source,
                            range.start().to_usize(),
                            span_len(range),
                        ));
                    }
                }
            }
            _ => {}
        }
    }
}

/// True when `expr` is the integer literal `0`.
fn is_int_zero_literal(expr: &Expr) -> bool {
    matches!(expr, Expr::NumberLiteral(n) if matches!(&n.value, Number::Int(i) if i.as_u64() == Some(0)))
}

/// Names possibly content-mutated within `body` (reassignment, subscript /
/// `del` store, or a mutating method call from `mutators`). Recurses into
/// every nested block **and** nested `def`/`class` body: over-counting only
/// ever *suppresses* an advice (the safe direction).
fn names_mutated(body: &[Stmt], mutators: &[&str]) -> HashSet<String> {
    struct V<'a> {
        out: HashSet<String>,
        mutators: &'a [&'a str],
    }
    impl<'ast, 'a> SourceOrderVisitor<'ast> for V<'a> {
        fn visit_expr(&mut self, e: &'ast Expr) {
            match e {
                Expr::Name(n) if matches!(n.ctx, ExprContext::Store | ExprContext::Del) => {
                    self.out.insert(n.id.to_string());
                }
                Expr::Subscript(s) if matches!(s.ctx, ExprContext::Store | ExprContext::Del) => {
                    if let Some(base) = base_name(&s.value) {
                        self.out.insert(base);
                    }
                }
                Expr::Call(c) => {
                    if let Expr::Attribute(a) = c.func.as_ref() {
                        if let Expr::Name(recv) = a.value.as_ref() {
                            if self.mutators.contains(&a.attr.as_str()) {
                                self.out.insert(recv.id.to_string());
                            }
                        }
                    }
                }
                _ => {}
            }
            walk_expr(self, e);
        }
    }
    let mut v = V {
        out: HashSet::new(),
        mutators,
    };
    for stmt in body {
        v.visit_stmt(stmt);
    }
    v.out
}

/// The root name of a (possibly nested) subscript / attribute target.
fn base_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name(n) => Some(n.id.to_string()),
        Expr::Subscript(s) => base_name(&s.value),
        Expr::Attribute(a) => base_name(&a.value),
        _ => None,
    }
}

/// Collect the bound names of a `for` target (a bare name or a nested
/// tuple/list/starred unpack).
fn collect_target_names(target: &Expr, out: &mut HashSet<String>) {
    match target {
        Expr::Name(n) => {
            out.insert(n.id.to_string());
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

// ── anywhere lints (5 & 6) ────────────────────────────────────────────────────

/// Visitor for the lints that are safe in any position (not loop-scoped):
/// `sorted(...)[0]`/`[-1]` (lint 5) and `k in d.keys()` (lint 6).
struct AnywhereVisitor<'a> {
    path: &'a str,
    source: &'a str,
    diags: &'a mut Diagnostics,
}

impl<'ast, 'a> SourceOrderVisitor<'ast> for AnywhereVisitor<'a> {
    fn visit_expr(&mut self, e: &'ast Expr) {
        // Lint 5 — `sorted(EXPR)[0]` / `sorted(EXPR)[-1]`, bare form only
        // (no `key=` / `reverse=` keywords).
        if let Expr::Subscript(sub) = e {
            if let Expr::Call(call) = sub.value.as_ref() {
                let is_bare_sorted = matches!(call.func.as_ref(), Expr::Name(n) if n.id.as_str() == "sorted")
                    && call.arguments.keywords.is_empty();
                if is_bare_sorted {
                    if let Some((which, builtin)) = first_or_last_index(&sub.slice) {
                        let range = e.range();
                        self.diags.push_warning(TycError::perf_sorted_first(
                            which,
                            builtin,
                            self.path,
                            self.source,
                            range.start().to_usize(),
                            span_len(range),
                        ));
                    }
                }
            }
        }
        // Lint 6 — `EXPR in NAME.keys()` / `not in NAME.keys()`. `EXPR`
        // (the tested element) must not be a bare constant literal: a
        // `"a" in d.keys()` shape is demonstration-y, and testing a
        // *variable* against keys is the case worth nudging.
        if let Expr::Compare(c) = e {
            let mut left: &Expr = &c.left;
            for (op, comparator) in c.ops.iter().zip(c.comparators.iter()) {
                if matches!(op, CmpOp::In | CmpOp::NotIn)
                    && is_keys_call(comparator)
                    && !is_constant_literal(left)
                {
                    let range = comparator.range();
                    self.diags.push_warning(TycError::perf_keys_membership(
                        self.path,
                        self.source,
                        range.start().to_usize(),
                        span_len(range),
                    ));
                }
                left = comparator;
            }
        }
        walk_expr(self, e);
    }
}

/// Classify a subscript index as `[0]` (→ `min`) or `[-1]` (→ `max`),
/// returning the `(subscript-form, suggested-builtin)` pair.
fn first_or_last_index(slice: &Expr) -> Option<(&'static str, &'static str)> {
    match slice {
        Expr::NumberLiteral(n) if matches!(&n.value, Number::Int(i) if i.as_u64() == Some(0)) => {
            Some(("[0]", "min"))
        }
        Expr::UnaryOp(u) if matches!(u.op, UnaryOp::USub) => match u.operand.as_ref() {
            Expr::NumberLiteral(n) if matches!(&n.value, Number::Int(i) if i.as_u64() == Some(1)) => {
                Some(("[-1]", "max"))
            }
            _ => None,
        },
        _ => None,
    }
}

/// True when `expr` is a bare constant literal (string / number / bytes /
/// bool / `None` / f-string / `...`).
fn is_constant_literal(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::StringLiteral(_)
            | Expr::NumberLiteral(_)
            | Expr::BytesLiteral(_)
            | Expr::BooleanLiteral(_)
            | Expr::NoneLiteral(_)
            | Expr::FString(_)
            | Expr::EllipsisLiteral(_)
    )
}

/// True when `expr` is a `<something>.keys()` call (no arguments).
fn is_keys_call(expr: &Expr) -> bool {
    if let Expr::Call(c) = expr {
        if c.arguments.args.is_empty() && c.arguments.keywords.is_empty() {
            if let Expr::Attribute(a) = c.func.as_ref() {
                return a.attr.as_str() == "keys";
            }
        }
    }
    false
}

// ── lazy import opportunity (lint 7) ──────────────────────────────────────────

/// Flag a module-level `import X` / `import X as Y` whose bound name is used
/// only inside function/method bodies, so declaring it `lazy import` would
/// defer the import cost from startup.
///
/// Conservative gates (any one suppresses the advice):
///   - not a single-name module-level `import` (from-imports never apply);
///   - the top-level module is in the Python 3.13 stdlib (deferring buys
///     nothing and risks shadowing);
///   - the bound name is already a `lazy import` alias, `pub`-exported, in a
///     hand-written `__all__`, or the file has a `pub *`;
///   - the file is an `__init__` (re-export surface);
///   - the module has an `if __name__ == "__main__":` guard — it's an
///     executable script whose imports load when it runs (the script *is*
///     the startup), so lazy imports there don't help;
///   - any reference to the bound name is *eager* (module/class scope, a
///     decorator, or a parameter / return annotation — all evaluated at
///     import time), or there are no references at all.
pub fn lazy_import_opportunity_diagnostics(
    module: &ModModule,
    path: &str,
    source: &str,
    ctx: &PerfLintContext,
    diags: &mut Diagnostics,
) {
    // `__init__` modules are a re-export surface; every import there is
    // conceptually public.
    if is_init_module(path) || ctx.has_pub_star {
        return;
    }
    // A module shaped like an executable script — a `__main__` guard or a
    // top-level `def main()` — has its imports loaded when the script runs,
    // so deferral gives no startup win. The lint's real target is a library
    // module (imported by others), where a heavy dependency touched only in
    // a rarely-called function genuinely benefits from deferral.
    if has_main_guard(&module.body) || defines_top_level_main(&module.body) {
        return;
    }
    let all_names = module_all_names(&module.body);
    let lazy: HashSet<&str> = ctx.lazy_import_aliases.iter().map(|s| s.as_str()).collect();
    let pubs: HashSet<&str> = ctx.pub_names.iter().map(|s| s.as_str()).collect();

    for stmt in &module.body {
        let Stmt::Import(imp) = stmt else {
            continue;
        };
        // Only single-name imports (`import a, b` is unusual; stay
        // conservative and skip it).
        if imp.names.len() != 1 {
            continue;
        }
        let alias = &imp.names[0];
        let module_name = alias.name.as_str();
        let top_level = module_name.split('.').next().unwrap_or(module_name);
        let bound = alias
            .asname
            .as_ref()
            .map(|n| n.as_str())
            .unwrap_or(top_level);

        if is_stdlib_top_level(top_level) {
            continue;
        }
        if lazy.contains(bound) || pubs.contains(bound) {
            continue;
        }
        if let Some(all) = &all_names {
            if all.contains(&bound) {
                continue;
            }
        }

        // Reference census: every reference to `bound` must be deferred
        // (inside a function body) and there must be at least one.
        let mut census = RefCensus {
            bound,
            eager: 0,
            deferred: 0,
        };
        census.scan(&module.body, false);
        if census.eager == 0 && census.deferred > 0 {
            let range = imp.range();
            diags.push_warning(TycError::lazy_import_opportunity(
                module_name,
                path,
                source,
                range.start().to_usize(),
                span_len(range),
            ));
        }
    }
}

/// Counts references to a bound name, split by whether each is evaluated
/// eagerly (module / class scope, or a def signature at those scopes — all
/// run at import time) or deferred (inside a function body).
struct RefCensus<'a> {
    bound: &'a str,
    eager: usize,
    deferred: usize,
}

impl RefCensus<'_> {
    fn scan(&mut self, stmts: &[Stmt], inside_fn_body: bool) {
        for stmt in stmts {
            self.scan_stmt(stmt, inside_fn_body);
        }
    }

    fn scan_stmt(&mut self, stmt: &Stmt, inside_fn_body: bool) {
        match stmt {
            Stmt::FunctionDef(f) => {
                // The signature (decorators, parameter annotations/defaults,
                // return annotation) is evaluated where the `def` is created —
                // eager unless this def is itself nested in a function body.
                for d in &f.decorator_list {
                    self.count_expr(&d.expression, inside_fn_body);
                }
                self.count_params(&f.parameters, inside_fn_body);
                if let Some(ret) = f.returns.as_deref() {
                    self.count_expr(ret, inside_fn_body);
                }
                self.scan(&f.body, true);
            }
            Stmt::ClassDef(c) => {
                for d in &c.decorator_list {
                    self.count_expr(&d.expression, inside_fn_body);
                }
                if let Some(args) = c.arguments.as_deref() {
                    for a in &args.args {
                        self.count_expr(a, inside_fn_body);
                    }
                    for k in &args.keywords {
                        self.count_expr(&k.value, inside_fn_body);
                    }
                }
                // The class body runs in the current context.
                self.scan(&c.body, inside_fn_body);
            }
            Stmt::If(s) => {
                self.count_expr(&s.test, inside_fn_body);
                self.scan(&s.body, inside_fn_body);
                for cl in &s.elif_else_clauses {
                    if let Some(t) = &cl.test {
                        self.count_expr(t, inside_fn_body);
                    }
                    self.scan(&cl.body, inside_fn_body);
                }
            }
            Stmt::While(s) => {
                self.count_expr(&s.test, inside_fn_body);
                self.scan(&s.body, inside_fn_body);
                self.scan(&s.orelse, inside_fn_body);
            }
            Stmt::For(s) => {
                self.count_expr(&s.iter, inside_fn_body);
                self.count_expr(&s.target, inside_fn_body);
                self.scan(&s.body, inside_fn_body);
                self.scan(&s.orelse, inside_fn_body);
            }
            Stmt::With(s) => {
                for item in &s.items {
                    self.count_expr(&item.context_expr, inside_fn_body);
                }
                self.scan(&s.body, inside_fn_body);
            }
            Stmt::Try(s) => {
                self.scan(&s.body, inside_fn_body);
                self.scan(&s.orelse, inside_fn_body);
                self.scan(&s.finalbody, inside_fn_body);
                for h in &s.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = h;
                    self.scan(&h.body, inside_fn_body);
                }
            }
            Stmt::Match(s) => {
                self.count_expr(&s.subject, inside_fn_body);
                for case in &s.cases {
                    self.scan(&case.body, inside_fn_body);
                }
            }
            other => {
                for e in stmt_all_exprs(other) {
                    self.count_expr(e, inside_fn_body);
                }
            }
        }
    }

    fn count_params(&mut self, params: &Parameters, inside_fn_body: bool) {
        for p in params
            .posonlyargs
            .iter()
            .chain(params.args.iter())
            .chain(params.kwonlyargs.iter())
        {
            if let Some(ann) = p.parameter.annotation.as_deref() {
                self.count_expr(ann, inside_fn_body);
            }
            if let Some(default) = p.default.as_deref() {
                self.count_expr(default, inside_fn_body);
            }
        }
        for extra in [params.vararg.as_deref(), params.kwarg.as_deref()]
            .into_iter()
            .flatten()
        {
            if let Some(ann) = extra.annotation.as_deref() {
                self.count_expr(ann, inside_fn_body);
            }
        }
    }

    fn count_expr(&mut self, expr: &Expr, inside_fn_body: bool) {
        let hits = count_name_refs(expr, self.bound);
        if inside_fn_body {
            self.deferred += hits;
        } else {
            self.eager += hits;
        }
    }
}

/// The direct expressions of a "simple" statement (used by the reference
/// census for statement kinds without their own arm).
fn stmt_all_exprs(stmt: &Stmt) -> Vec<&Expr> {
    match stmt {
        Stmt::Assign(s) => {
            let mut v: Vec<&Expr> = s.targets.iter().collect();
            v.push(&s.value);
            v
        }
        Stmt::AnnAssign(s) => {
            let mut v = vec![s.target.as_ref(), s.annotation.as_ref()];
            if let Some(val) = s.value.as_deref() {
                v.push(val);
            }
            v
        }
        Stmt::AugAssign(s) => vec![s.target.as_ref(), s.value.as_ref()],
        Stmt::Return(s) => s.value.as_deref().into_iter().collect(),
        Stmt::Expr(s) => vec![s.value.as_ref()],
        Stmt::Delete(s) => s.targets.iter().collect(),
        Stmt::Assert(s) => {
            let mut v = vec![s.test.as_ref()];
            if let Some(m) = s.msg.as_deref() {
                v.push(m);
            }
            v
        }
        Stmt::TypeAlias(s) => vec![s.value.as_ref()],
        _ => Vec::new(),
    }
}

/// Count `Name` references to `bound` anywhere within `expr`.
fn count_name_refs(expr: &Expr, bound: &str) -> usize {
    struct V<'a> {
        bound: &'a str,
        count: usize,
    }
    impl<'ast, 'a> SourceOrderVisitor<'ast> for V<'a> {
        fn visit_expr(&mut self, e: &'ast Expr) {
            if let Expr::Name(n) = e {
                if n.id.as_str() == self.bound {
                    self.count += 1;
                }
            }
            walk_expr(self, e);
        }
    }
    let mut v = V { bound, count: 0 };
    v.visit_expr(expr);
    v.count
}

/// True when the module defines a top-level `def main` / `async def main`.
fn defines_top_level_main(body: &[Stmt]) -> bool {
    body.iter()
        .any(|s| matches!(s, Stmt::FunctionDef(f) if f.name.as_str() == "main"))
}

/// True when the module contains a top-level `if __name__ == "__main__":`
/// (either operand order).
fn has_main_guard(body: &[Stmt]) -> bool {
    fn is_name_eq_main(a: &Expr, b: &Expr) -> bool {
        matches!(a, Expr::Name(n) if n.id.as_str() == "__name__")
            && matches!(b, Expr::StringLiteral(s) if s.value.to_str() == "__main__")
    }
    body.iter().any(|stmt| {
        let Stmt::If(s) = stmt else {
            return false;
        };
        let Expr::Compare(c) = s.test.as_ref() else {
            return false;
        };
        if c.ops.len() != 1 || !matches!(c.ops[0], CmpOp::Eq) {
            return false;
        }
        let Some(right) = c.comparators.first() else {
            return false;
        };
        is_name_eq_main(c.left.as_ref(), right) || is_name_eq_main(right, c.left.as_ref())
    })
}

/// The names in a hand-written module-level `__all__ = [...]` / `(...)`, if
/// present.
fn module_all_names(body: &[Stmt]) -> Option<HashSet<&str>> {
    for stmt in body {
        let value = match stmt {
            Stmt::Assign(a)
                if a.targets.len() == 1
                    && matches!(&a.targets[0], Expr::Name(n) if n.id.as_str() == "__all__") =>
            {
                a.value.as_ref()
            }
            Stmt::AnnAssign(a) if matches!(a.target.as_ref(), Expr::Name(n) if n.id.as_str() == "__all__") => {
                match a.value.as_deref() {
                    Some(v) => v,
                    None => continue,
                }
            }
            _ => continue,
        };
        let elts = match value {
            Expr::List(l) => &l.elts,
            Expr::Tuple(t) => &t.elts,
            _ => continue,
        };
        let mut out = HashSet::new();
        for e in elts {
            if let Expr::StringLiteral(s) = e {
                out.insert(s.value.to_str());
            }
        }
        return Some(out);
    }
    None
}

/// True when `path`'s file stem is `__init__`.
fn is_init_module(path: &str) -> bool {
    std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s == "__init__")
        .unwrap_or(false)
}

// ── shared helpers ────────────────────────────────────────────────────────────

fn span_len(range: TextRange) -> usize {
    range
        .end()
        .to_usize()
        .saturating_sub(range.start().to_usize())
        .max(1)
}

#[cfg(test)]
mod tests;
