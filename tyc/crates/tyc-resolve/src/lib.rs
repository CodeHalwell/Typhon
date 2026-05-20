//! Name resolution and scope construction for Typhon.
//!
//! Walks a parsed Python module and produces:
//!
//! - A tree of [`Scope`]s rooted at the module scope.
//! - A [`SymbolTable`] that maps every introduced name to its declaration.
//! - A set of [`Reference`]s recording each use of a name.
//! - Diagnostics for unknown names and `let` re-assignments.
//!
//! The resolver consumes the original Typhon source plus the parsed Python
//! AST. The Python AST has byte offsets relative to the *preprocessed*
//! source, but the let/mut stripping never alters line numbers and only
//! removes characters at the start of a line, so positions inside
//! expressions remain stable; we use them directly.

use ruff_python_ast::{self as ast, Expr, ModModule, Stmt};
use ruff_text_size::{Ranged, TextRange};
use tyc_diagnostics::{Diagnostics, TycError};

/// Mutability of a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutability {
    /// `let` — immutable; reassignment is a compile error.
    Let,
    /// `mut`, function/class declaration, parameter, or import — mutable
    /// or rebindable by the language semantics. Only `let` is rejected on
    /// reassignment.
    Mut,
}

/// What kind of entity a binding introduces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingKind {
    /// A `let` or `mut` value binding (annotated or not).
    Value,
    /// A `def` function definition.
    Function,
    /// A `class` definition.
    Class,
    /// A function parameter.
    Parameter,
    /// An imported name.
    Import,
    /// A bound `for`/`with`/`except`/`comprehension` target.
    Loop,
}

/// Sub-kind for `BindingKind::Class`. Plain `class` is a dataclass at emit
/// time; `class!` opts out of that and may carry a synthesised or hand-
/// written `__init__` that calls `super().__init__()`.
///
/// Only populated for `BindingKind::Class` bindings; every other kind
/// uses the default [`ClassKind::Plain`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClassKind {
    /// Default `class Foo:` — emits as `@dataclass(slots=True) class Foo:`.
    #[default]
    Plain,
    /// `class! Foo(Base):` — raw class, no dataclass decorator. The
    /// desugar pass may synthesise an `__init__` that calls
    /// `super().__init__()` before assigning declared fields.
    Raw,
}

/// One name introduced in some scope.
#[derive(Debug, Clone)]
pub struct Binding {
    pub name: String,
    pub kind: BindingKind,
    pub mutability: Mutability,
    /// Byte range of the declaration site in the preprocessed source.
    pub span: (usize, usize),
    /// For `BindingKind::Import` bindings, carries the imported module
    /// path and (for `from`-imports) the original symbol name so cross-file
    /// go-to-definition can resolve `pkg.util.frobnicate` back to the
    /// originating `.ty` source.  `None` for non-import bindings.
    pub import_info: Option<ImportInfo>,
    /// For `BindingKind::Class` bindings, indicates whether the source
    /// declared the class as `class!` (raw) or plain `class`. Other
    /// binding kinds always carry [`ClassKind::Plain`].
    pub class_kind: ClassKind,
}

/// Origin metadata for an import binding, used by the LSP backend to drive
/// cross-file go-to-definition.
#[derive(Debug, Clone)]
pub struct ImportInfo {
    /// Dotted Python module path the symbol was sourced from.
    /// `import pkg.util`        → `pkg.util`
    /// `from pkg.util import f` → `pkg.util`
    pub module: String,
    /// For `from … import name` (or `from … import name as alias`), the
    /// original member name. `None` for bare `import` statements where
    /// the bound name is the module itself.
    pub member: Option<String>,
}

impl Binding {
    pub fn span_offset(&self) -> usize {
        self.span.0
    }

    pub fn span_length(&self) -> usize {
        self.span.1.saturating_sub(self.span.0)
    }
}

/// One use of a name in some scope.
#[derive(Debug, Clone)]
pub struct Reference {
    pub name: String,
    /// Byte range of the reference in the preprocessed source.
    pub span: (usize, usize),
    /// Index of the scope in which the reference appears.
    pub scope: ScopeId,
}

/// Kind of a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Module,
    Function,
    Class,
    Comprehension,
}

pub type ScopeId = usize;

/// One scope in the program (module, function body, class body, …).
#[derive(Debug, Clone)]
pub struct Scope {
    pub id: ScopeId,
    pub kind: ScopeKind,
    pub parent: Option<ScopeId>,
    pub bindings: Vec<Binding>,
    /// Byte range covered by this scope in the preprocessed source.  The
    /// module scope spans the entire file; function/class/lambda/
    /// comprehension scopes span their AST node's range.  Used by
    /// [`ResolvedModule::scope_at_offset`] to drive LSP completion.
    pub span: (usize, usize),
}

impl Scope {
    pub fn lookup_local(&self, name: &str) -> Option<&Binding> {
        self.bindings.iter().find(|b| b.name == name)
    }

    /// `true` when `offset` lies inside this scope's byte range.
    pub fn contains_offset(&self, offset: usize) -> bool {
        offset >= self.span.0 && offset < self.span.1
    }
}

/// Options passed to [`resolve_module_with`] so downstream stages can
/// surface Typhon-specific syntax that the Python AST has already lost.
///
/// Today only `raw_class_byte_starts` is carried; future fields (e.g.
/// `interface_byte_starts`, `model_byte_starts`) will likely use the
/// same pattern instead of widening the resolver's public signature.
#[derive(Debug, Clone, Default)]
pub struct ResolveOptions {
    /// Sorted byte offsets (in the preprocessed source) at the start of
    /// each `class!` declaration line. A class declaration whose stmt
    /// range starts at or after one of these offsets — and before the
    /// next line break — is tagged [`ClassKind::Raw`]. The preprocessor
    /// emits this list via [`tyc_syntax::preprocess::line_byte_starts`].
    pub raw_class_byte_starts: Vec<u32>,
    /// Lazy-import diagnostic remaps. When the unused-import pass fires
    /// on a binding whose preprocessed line was originally a `lazy
    /// import` declaration, the resolver rewrites the diagnostic's
    /// source and span to point at the user-authored
    /// `lazy import ALIAS = MODULE` line instead of the
    /// preprocessor-synthesised `import MODULE as ALIAS` (FINDINGS #15).
    /// Empty for callers that don't have the metadata handy.
    pub lazy_import_remaps: Vec<LazyImportRemap>,
    /// The original Typhon source (pre-preprocess). Required when
    /// `lazy_import_remaps` is non-empty so the diagnostic can render
    /// the user-written line. Ignored when no remaps are present.
    pub original_source: Option<String>,
}

/// One `lazy import ALIAS = MODULE` declaration's mapping back to the
/// original Typhon source. Built by callers from
/// [`tyc_syntax::preprocess::PreprocessResult::lazy_imports`] plus the
/// raw text and surfaced to the resolver through [`ResolveOptions`].
#[derive(Debug, Clone)]
pub struct LazyImportRemap {
    /// The 0-based line index of the `lazy import` statement. The
    /// preprocessor preserves line numbering exactly (each `lazy import`
    /// line becomes one `import X as Y` line), so this same index
    /// applies to both the original Typhon source and the preprocessed
    /// Python source the resolver walks.
    pub line_index: usize,
    /// Byte offset in the *original* source of the alias identifier
    /// (the `np` in `lazy import np = numpy`). This is where the
    /// diagnostic's label anchors.
    pub original_alias_offset: usize,
    /// Length of the alias identifier in bytes.
    pub original_alias_length: usize,
}

/// The resolved structure of a module: scopes, bindings, references, plus
/// the list of `(declaration_offset, mutability)` pairs for every binding
/// so the type checker can find them again later.
#[derive(Debug, Clone, Default)]
pub struct ResolvedModule {
    pub scopes: Vec<Scope>,
    pub references: Vec<Reference>,
}

/// Curated list of Python stdlib top-level module names (root names only;
/// `os.path` is covered by the `os` entry). Used by
/// [`check_unknown_modules`] to vet `from X import Y` / `import X` at
/// check time so a typoed or missing-dep module surfaces before `tyc
/// build` runs the program. This list is intentionally a static snapshot
/// of the CPython 3.13 stdlib root-names — the LSP autocomplete table
/// covers the depth needed for member resolution, but we only need root
/// matches here.
pub fn python_stdlib_modules() -> &'static [&'static str] {
    &[
        "__future__",
        "_thread",
        "abc",
        "aifc",
        "argparse",
        "array",
        "ast",
        "asynchat",
        "asyncio",
        "asyncore",
        "atexit",
        "audioop",
        "base64",
        "bdb",
        "binascii",
        "bisect",
        "builtins",
        "bz2",
        "calendar",
        "cgi",
        "cgitb",
        "chunk",
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
        "crypt",
        "csv",
        "ctypes",
        "curses",
        "dataclasses",
        "datetime",
        "dbm",
        "decimal",
        "difflib",
        "dis",
        "distutils",
        "doctest",
        "email",
        "encodings",
        "ensurepip",
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
        "genericpath",
        "getopt",
        "getpass",
        "gettext",
        "glob",
        "graphlib",
        "grp",
        "gzip",
        "hashlib",
        "heapq",
        "hmac",
        "html",
        "http",
        "idlelib",
        "imaplib",
        "imghdr",
        "imp",
        "importlib",
        "inspect",
        "io",
        "ipaddress",
        "itertools",
        "json",
        "keyword",
        "lib2to3",
        "linecache",
        "locale",
        "logging",
        "lzma",
        "mailbox",
        "mailcap",
        "marshal",
        "math",
        "mimetypes",
        "mmap",
        "modulefinder",
        "msilib",
        "msvcrt",
        "multiprocessing",
        "netrc",
        "nis",
        "nntplib",
        "ntpath",
        "numbers",
        "opcode",
        "operator",
        "optparse",
        "os",
        "ossaudiodev",
        "parser",
        "pathlib",
        "pdb",
        "pickle",
        "pickletools",
        "pipes",
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
        "smtpd",
        "smtplib",
        "sndhdr",
        "socket",
        "socketserver",
        "spwd",
        "sqlite3",
        "sre_compile",
        "sre_constants",
        "sre_parse",
        "ssl",
        "stat",
        "statistics",
        "string",
        "stringprep",
        "struct",
        "subprocess",
        "sunau",
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
        "turtledemo",
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
    ]
}

/// Vet a module's imports against a set of resolvable module names and
/// emit `tyc::unknown_module` warnings for any unresolvable root.
///
/// `project_modules` should contain the dotted-name form of every `.ty`
/// file in the project (`src/main.ty` → `"main"`, `src/pkg/sub.ty` →
/// `"pkg.sub"`). The function compares the *root* of each imported
/// module against:
///
/// - the Python stdlib whitelist returned by [`python_stdlib_modules`],
/// - the Typhon-bundled `typhon_runtime` package,
/// - any project module whose dotted-name has the import's root as a
///   prefix segment (so an `import pkg` resolves both `pkg/__init__.ty`
///   and `pkg.sub` projects),
/// - the optional `extra_modules` list, which `tyc check` populates from
///   `typhon.toml` dependencies plus a permissive fallback for
///   third-party packages (anything explicitly listed is assumed to
///   resolve at runtime).
///
/// Unknown roots produce a warning (not an error) so existing programs
/// that depend on quietly-installed sibling packages keep building;
/// callers can promote the warning via strictness if desired. FINDINGS #79.
pub fn check_unknown_modules(
    path: &str,
    source: &str,
    module: &ruff_python_ast::ModModule,
    project_modules: &[String],
    extra_modules: &[String],
) -> Diagnostics {
    use ruff_python_ast::Stmt;

    let mut diags = Diagnostics::new();
    let stdlib: std::collections::HashSet<&str> = python_stdlib_modules().iter().copied().collect();
    let project_roots: std::collections::HashSet<&str> = project_modules
        .iter()
        .map(|m| m.split('.').next().unwrap_or(m.as_str()))
        .collect();
    let extra_roots: std::collections::HashSet<&str> = extra_modules
        .iter()
        .map(|m| m.split('.').next().unwrap_or(m.as_str()))
        .collect();
    let is_resolvable = |module_name: &str| -> bool {
        let root = module_name.split('.').next().unwrap_or(module_name);
        if root.is_empty() || root.starts_with('_') {
            // Bare `from . import sibling`, dunder names, or relative imports —
            // not vettable here, trust the build.
            return true;
        }
        root == "typhon_runtime"
            || stdlib.contains(root)
            || project_roots.contains(root)
            || extra_roots.contains(root)
    };

    fn walk(
        stmts: &[Stmt],
        path: &str,
        source: &str,
        is_resolvable: &dyn Fn(&str) -> bool,
        diags: &mut Diagnostics,
    ) {
        for stmt in stmts {
            match stmt {
                Stmt::Import(imp) => {
                    for alias in &imp.names {
                        let module_name = alias.name.as_str();
                        if !is_resolvable(module_name) {
                            let span =
                                (alias.range.start().to_usize(), alias.range.end().to_usize());
                            let length = span.1.saturating_sub(span.0).max(1);
                            diags.push_warning(TycError::unknown_module(
                                module_name,
                                path,
                                source,
                                span.0,
                                length,
                            ));
                        }
                    }
                }
                Stmt::ImportFrom(imp) => {
                    if imp.level > 0 {
                        // Relative imports (`from .sibling import X`) —
                        // skip; we don't model relative path resolution
                        // at this layer.
                        continue;
                    }
                    if let Some(module_name) = imp.module.as_ref() {
                        let name = module_name.as_str();
                        if !is_resolvable(name) {
                            let span = (
                                module_name.range.start().to_usize(),
                                module_name.range.end().to_usize(),
                            );
                            let length = span.1.saturating_sub(span.0).max(1);
                            diags.push_warning(TycError::unknown_module(
                                name, path, source, span.0, length,
                            ));
                        }
                    }
                }
                Stmt::FunctionDef(f) => walk(&f.body, path, source, is_resolvable, diags),
                Stmt::ClassDef(c) => walk(&c.body, path, source, is_resolvable, diags),
                Stmt::If(s) => {
                    walk(&s.body, path, source, is_resolvable, diags);
                    for c in &s.elif_else_clauses {
                        walk(&c.body, path, source, is_resolvable, diags);
                    }
                }
                Stmt::Try(s) => {
                    walk(&s.body, path, source, is_resolvable, diags);
                    walk(&s.orelse, path, source, is_resolvable, diags);
                    walk(&s.finalbody, path, source, is_resolvable, diags);
                    // Try handlers' bodies aren't otherwise reached — an
                    // `import X` inside an `except ImportError:` block is
                    // legitimate and must be vetted.
                    for handler in &s.handlers {
                        let ruff_python_ast::ExceptHandler::ExceptHandler(h) = handler;
                        walk(&h.body, path, source, is_resolvable, diags);
                    }
                }
                // Loop / context-manager / pattern-match bodies can carry
                // imports too — `with importlib.util.LazyLoader(...): import x`
                // is unusual but legal Python. Walk every nested body so
                // missed module diagnostics fire consistently regardless of
                // surrounding statement kind. (gemini-code-assist review on
                // PR #68, file tyc-resolve/src/lib.rs L382.)
                Stmt::For(s) => {
                    walk(&s.body, path, source, is_resolvable, diags);
                    walk(&s.orelse, path, source, is_resolvable, diags);
                }
                Stmt::While(s) => {
                    walk(&s.body, path, source, is_resolvable, diags);
                    walk(&s.orelse, path, source, is_resolvable, diags);
                }
                Stmt::With(s) => walk(&s.body, path, source, is_resolvable, diags),
                Stmt::Match(s) => {
                    for case in &s.cases {
                        walk(&case.body, path, source, is_resolvable, diags);
                    }
                }
                _ => {}
            }
        }
    }

    walk(&module.body, path, source, &is_resolvable, &mut diags);
    diags
}

impl ResolvedModule {
    pub fn module_scope(&self) -> &Scope {
        &self.scopes[0]
    }

    /// Walk the scope chain starting at `scope` and return the first
    /// binding matching `name`, plus the scope it was found in.
    pub fn lookup<'a>(&'a self, scope: ScopeId, name: &str) -> Option<(&'a Binding, ScopeId)> {
        let mut current = Some(scope);
        while let Some(id) = current {
            let s = &self.scopes[id];
            if let Some(b) = s.lookup_local(name) {
                return Some((b, id));
            }
            current = s.parent;
        }
        None
    }

    /// Iterator over every binding declared in any scope, paired with the
    /// scope id it belongs to.  Useful for go-to-definition lookups.
    pub fn all_bindings(&self) -> impl Iterator<Item = (ScopeId, &Binding)> {
        self.scopes
            .iter()
            .flat_map(|s| s.bindings.iter().map(move |b| (s.id, b)))
    }

    /// Innermost scope whose byte range contains `offset`.  Falls back to
    /// the module scope when no narrower scope matches (e.g. an offset that
    /// sits between two top-level statements).
    ///
    /// Used by the LSP backend to drive completion: walk the parent chain
    /// from this scope upward and collect every visible binding.
    pub fn scope_at_offset(&self, offset: usize) -> ScopeId {
        // Scopes are pushed in source order, so a deeper (later) scope that
        // contains the offset is strictly the innermost match. Iterate from
        // the end to find it without building a tree.
        for s in self.scopes.iter().rev() {
            if s.contains_offset(offset) {
                return s.id;
            }
        }
        0
    }

    /// Every binding visible from `scope` (its own bindings plus those
    /// inherited from parent scopes).  Walks the parent chain to the
    /// module scope; later definitions in nested scopes shadow earlier
    /// ones with the same name.
    pub fn visible_bindings(&self, scope: ScopeId) -> Vec<&Binding> {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut out: Vec<&Binding> = Vec::new();
        let mut current = Some(scope);
        while let Some(id) = current {
            let s = &self.scopes[id];
            for b in &s.bindings {
                if seen.insert(b.name.as_str()) {
                    out.push(b);
                }
            }
            current = s.parent;
        }
        out
    }

    /// Find the identifier (binding or reference) at the given byte offset
    /// in the preprocessed source.  Returns the symbol name plus, if
    /// resolvable, the corresponding binding (the definition site).
    ///
    /// Used by the LSP backend to implement hover and go-to-definition:
    /// hover renders the binding kind and declaration span; go-to
    /// jumps to the binding's offset.
    pub fn symbol_at_offset(&self, offset: usize) -> Option<SymbolAtOffset<'_>> {
        // Prefer references first — a binding's span overlaps the
        // identifier in the declaration site, but a reference is the more
        // useful match when the user clicks on a use.
        for r in &self.references {
            if r.span.0 <= offset && offset < r.span.1 {
                let definition = self.lookup(r.scope, &r.name).map(|(b, _)| b);
                return Some(SymbolAtOffset {
                    name: r.name.clone(),
                    span: r.span,
                    definition,
                    is_definition: false,
                });
            }
        }
        // Fall back to binding declaration sites.
        for (_, b) in self.all_bindings() {
            if b.span.0 <= offset && offset < b.span.1 {
                return Some(SymbolAtOffset {
                    name: b.name.clone(),
                    span: b.span,
                    definition: Some(b),
                    is_definition: true,
                });
            }
        }
        None
    }
}

/// What the resolver knows about an identifier at a given byte offset.
#[derive(Debug, Clone)]
pub struct SymbolAtOffset<'a> {
    /// The identifier text.
    pub name: String,
    /// Byte range of the identifier itself in the preprocessed source.
    pub span: (usize, usize),
    /// The binding the identifier refers to, when resolvable.  `None` for
    /// unresolved references (would also produce an "unknown name"
    /// diagnostic at check time).
    pub definition: Option<&'a Binding>,
    /// True when this offset lies inside a declaration site (`let x =`,
    /// `def foo`, `class Foo:`).
    pub is_definition: bool,
}

/// Internal helper for building a [`ResolvedModule`] while walking the AST.
struct Resolver<'a> {
    path: String,
    source: &'a str,
    scopes: Vec<Scope>,
    references: Vec<Reference>,
    diagnostics: Diagnostics,
    /// `(decl_span, new_span)` pairs already reported as `immutable_assign`.
    /// The resolver double-visits each body (pre-collect + walk_stmt), so
    /// without this guard a re-declaration would emit the same diagnostic
    /// twice.
    seen_immutable_redecl: std::collections::HashSet<((usize, usize), (usize, usize))>,
    /// `(scope, span)` pairs already reported as `missing_binding_kind`.
    /// Same dedup story as `seen_immutable_redecl`: the bareword
    /// assignment is visited once per pre-collect pass and once per
    /// walk pass, but we only want one diagnostic per source location.
    seen_missing_binding_kind: std::collections::HashSet<(ScopeId, (usize, usize))>,
    /// Names declared `global X` or `nonlocal X` per scope. Suppresses
    /// `tyc::missing_binding_kind` on later `X = …` assignments inside
    /// the same scope — the user has already told us *where* the binding
    /// lives, so insisting on a `let`/`mut` keyword is noise.
    /// FINDINGS #61.
    global_nonlocal_names: std::collections::HashMap<ScopeId, std::collections::HashSet<String>>,
    /// Sorted byte offsets pointing at the first non-whitespace character
    /// of each `class!` declaration line in [`Self::source`]. Consulted
    /// when declaring a class binding to decide whether to tag it
    /// [`ClassKind::Raw`].
    raw_class_byte_starts: Vec<u32>,
    /// Lazy-import remap metadata + the original Typhon source. When
    /// `lazy_import_remaps` is non-empty, the unused-import emitter
    /// rewrites its diagnostic to anchor on the original `lazy import
    /// ALIAS = MODULE` line (FINDINGS #15).
    lazy_import_remaps: Vec<LazyImportRemap>,
    original_source: Option<String>,
    /// Line starts (byte offsets) of the preprocessed source. Lazily
    /// computed the first time the unused-import emitter needs to
    /// translate a preprocessed byte offset to a line index.
    preprocessed_line_starts: std::cell::OnceCell<Vec<usize>>,
}

impl<'a> Resolver<'a> {
    fn new_with(path: String, source: &'a str, options: ResolveOptions) -> Self {
        let module = Scope {
            id: 0,
            kind: ScopeKind::Module,
            parent: None,
            bindings: Vec::new(),
            span: (0, source.len()),
        };
        Self {
            path,
            source,
            scopes: vec![module],
            references: Vec::new(),
            diagnostics: Diagnostics::new(),
            seen_immutable_redecl: std::collections::HashSet::new(),
            seen_missing_binding_kind: std::collections::HashSet::new(),
            global_nonlocal_names: std::collections::HashMap::new(),
            raw_class_byte_starts: options.raw_class_byte_starts,
            lazy_import_remaps: options.lazy_import_remaps,
            original_source: options.original_source,
            preprocessed_line_starts: std::cell::OnceCell::new(),
        }
    }

    /// True when the statement starting at `stmt_start` (a byte offset
    /// into [`Self::source`]) sits on a line that was originally written
    /// as `class!`. Cheap binary search over the sorted offsets list.
    fn is_raw_class_offset(&self, stmt_start: usize) -> bool {
        if self.raw_class_byte_starts.is_empty() {
            return false;
        }
        // Find the line start at or before `stmt_start`. A `class!` line
        // start is recorded as the first non-whitespace byte on the line,
        // which is exactly where the `class` keyword begins after the
        // preprocessor strips the `!`. The statement's range therefore
        // starts at that byte (no leading whitespace is part of the range).
        let target = stmt_start as u32;
        self.raw_class_byte_starts.binary_search(&target).is_ok()
    }

    fn push_scope(&mut self, kind: ScopeKind, parent: ScopeId, span: (usize, usize)) -> ScopeId {
        let id = self.scopes.len();
        self.scopes.push(Scope {
            id,
            kind,
            parent: Some(parent),
            bindings: Vec::new(),
            span,
        });
        id
    }

    /// Has a binding called `name` already been declared in `scope`?
    fn lookup_local(&self, scope: ScopeId, name: &str) -> Option<&Binding> {
        self.scopes[scope].lookup_local(name)
    }

    fn declare(
        &mut self,
        scope: ScopeId,
        name: &str,
        kind: BindingKind,
        mutability: Mutability,
        span: (usize, usize),
    ) {
        self.declare_full(scope, name, kind, mutability, span, None, ClassKind::Plain);
    }

    fn declare_with(
        &mut self,
        scope: ScopeId,
        name: &str,
        kind: BindingKind,
        mutability: Mutability,
        span: (usize, usize),
        import_info: Option<ImportInfo>,
    ) {
        self.declare_full(
            scope,
            name,
            kind,
            mutability,
            span,
            import_info,
            ClassKind::Plain,
        );
    }

    /// Declare a class binding with an explicit [`ClassKind`]. Used for
    /// `class!` declarations so downstream queries (LSP hover, migrate,
    /// future cross-module checks) can see the raw-class marker on the
    /// binding metadata itself rather than re-scanning byte offsets.
    fn declare_class(
        &mut self,
        scope: ScopeId,
        name: &str,
        span: (usize, usize),
        class_kind: ClassKind,
    ) {
        self.declare_full(
            scope,
            name,
            BindingKind::Class,
            Mutability::Mut,
            span,
            None,
            class_kind,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn declare_full(
        &mut self,
        scope: ScopeId,
        name: &str,
        kind: BindingKind,
        mutability: Mutability,
        span: (usize, usize),
        import_info: Option<ImportInfo>,
        class_kind: ClassKind,
    ) {
        if let Some(existing) = self.lookup_local(scope, name) {
            // Re-entry at exactly the same span is just the same statement
            // being visited twice (e.g. pre-collect followed by walk_stmt);
            // silently no-op.
            if existing.span == span {
                return;
            }
            // Otherwise this is a re-declaration. Forbid it whenever either
            // side is `val`, regardless of binding kind: rebinding a `val`
            // via `def`, `class`, a for-loop target, or another assignment
            // all violate immutability.
            let _ = kind;
            if existing.mutability == Mutability::Let || mutability == Mutability::Let {
                let decl_span = existing.span;
                if self.seen_immutable_redecl.insert((decl_span, span)) {
                    self.diagnostics.push_error(TycError::immutable_assign(
                        name,
                        &self.path,
                        self.source,
                        decl_span.0,
                        decl_span.1.saturating_sub(decl_span.0).max(1),
                        span.0,
                        span.1.saturating_sub(span.0).max(1),
                    ));
                }
                return;
            }
            // Non-val rebinding: silently keep the first declaration.
            return;
        }

        self.scopes[scope].bindings.push(Binding {
            name: name.to_owned(),
            kind,
            mutability,
            span,
            import_info,
            class_kind,
        });
    }

    fn reference(&mut self, scope: ScopeId, name: &str, span: (usize, usize)) {
        self.references.push(Reference {
            name: name.to_owned(),
            span,
            scope,
        });
    }

    fn report_unknown_names(&mut self) {
        let builtins = builtin_names();
        for r in &self.references {
            // Walk the scope chain.
            let mut found = false;
            let mut current = Some(r.scope);
            while let Some(id) = current {
                let scope = &self.scopes[id];
                if scope.bindings.iter().any(|b| b.name == r.name) {
                    found = true;
                    break;
                }
                current = scope.parent;
            }
            if !found && !builtins.contains(&r.name.as_str()) {
                let length = r.span.1.saturating_sub(r.span.0).max(1);
                // `self` is special: it's only legal inside an `impl`
                // method body, so an unresolved reference deserves a
                // dedicated diagnostic that explains the rule rather
                // than the generic "declare with `let`/`mut`" help
                // (which would mislead the user — `let self = ...`
                // does not solve the problem). FINDINGS #90.
                if r.name == "self" {
                    self.diagnostics.push_error(TycError::self_outside_impl(
                        &self.path,
                        self.source,
                        r.span.0,
                        length,
                    ));
                } else {
                    self.diagnostics.push_error(TycError::unknown_name(
                        r.name.clone(),
                        &self.path,
                        self.source,
                        r.span.0,
                        length,
                    ));
                }
            }
        }
    }

    fn report_unused_imports(&mut self) {
        // Resolve each reference to the specific binding it refers to (by
        // walking the scope chain, exactly like report_unknown_names does).
        // This correctly handles name shadowing: a local `os` parameter does
        // not mark a module-level `import os` as used.
        let mut used_bindings: std::collections::HashSet<(usize, usize)> =
            std::collections::HashSet::new();

        for r in &self.references {
            let mut current = Some(r.scope);
            while let Some(id) = current {
                let scope = &self.scopes[id];
                if let Some(idx) = scope.bindings.iter().position(|b| b.name == r.name) {
                    used_bindings.insert((id, idx));
                    break;
                }
                current = scope.parent;
            }
        }

        for (scope_id, scope) in self.scopes.iter().enumerate() {
            for (binding_idx, binding) in scope.bindings.iter().enumerate() {
                if binding.kind != BindingKind::Import {
                    continue;
                }
                // Wildcard imports (`from foo import *`) cannot be checked.
                if binding.name == "*" {
                    continue;
                }
                // `_`-prefixed names are conventionally "intentionally unused".
                if binding.name.starts_with('_') {
                    continue;
                }
                if !used_bindings.contains(&(scope_id, binding_idx)) {
                    // FINDINGS #15: when this import sits on a line that
                    // was originally `lazy import ALIAS = MODULE`, render
                    // the diagnostic against the original Typhon source
                    // and anchor on the user-written alias offset.
                    if let Some(remap) = self.lazy_import_remap_for(binding.span.0) {
                        if let Some(orig) = self.original_source.as_ref() {
                            self.diagnostics.push_warning(TycError::unused_import(
                                binding.name.clone(),
                                &self.path,
                                orig.clone(),
                                remap.original_alias_offset,
                                remap.original_alias_length.max(1),
                            ));
                            continue;
                        }
                    }
                    let length = binding.span_length().max(1);
                    self.diagnostics.push_warning(TycError::unused_import(
                        binding.name.clone(),
                        &self.path,
                        self.source,
                        binding.span.0,
                        length,
                    ));
                }
            }
        }
    }

    /// Advice-level diagnostic for FINDINGS #92: a top-level
    /// `def main() -> None:` is defined but `main` is never
    /// referenced anywhere in the module. The script will compile
    /// and run without output, which is almost always a mistake —
    /// the canonical Python entry pattern is
    /// `if __name__ == "__main__": main()`.
    ///
    /// Suppressed when `main` is referenced (even from a comment-
    /// stripped `if` block), or when the module also defines a
    /// classifier name like `__all__` that would suggest a library
    /// shape (in which case the user is exporting `main` rather
    /// than running it).
    fn report_main_not_called(&mut self) {
        // Find the module-level `main` binding, if any. Module scope is
        // scope id 0.
        let module_scope = &self.scopes[0];
        let main_binding = match module_scope
            .bindings
            .iter()
            .find(|b| b.name == "main" && b.kind == BindingKind::Function)
        {
            Some(b) => b.clone(),
            None => return,
        };
        // Suppress when the module looks like a library (has `__all__`).
        if module_scope.bindings.iter().any(|b| b.name == "__all__") {
            return;
        }
        // Any reference to `main` other than the def site counts as a
        // use. References track only call sites and bare-name reads,
        // not the def site itself.
        let has_use = self
            .references
            .iter()
            .any(|r| r.name == "main" && r.span != main_binding.span);
        if has_use {
            return;
        }
        let length = main_binding
            .span
            .1
            .saturating_sub(main_binding.span.0)
            .max(1);
        // Stored as a warning so it flows through the existing
        // Diagnostics channels; the diagnostic itself carries
        // `severity(Advice)` so miette renders it as advice rather
        // than warning when displayed.
        self.diagnostics.push_warning(TycError::main_not_called(
            &self.path,
            self.source,
            main_binding.span.0,
            length,
        ));
    }

    /// Translate a preprocessed-source byte offset to a 0-based line
    /// index, computing (and caching) the line-start table on first
    /// use. Lazily computed because most resolves don't need it.
    fn preprocessed_line_at_offset(&self, offset: usize) -> usize {
        let starts = self.preprocessed_line_starts.get_or_init(|| {
            let mut v = vec![0usize];
            for (i, b) in self.source.bytes().enumerate() {
                if b == b'\n' {
                    v.push(i + 1);
                }
            }
            v
        });
        match starts.binary_search(&offset) {
            Ok(line) => line,
            Err(line) => line.saturating_sub(1),
        }
    }

    /// If `offset` (preprocessed) lies on a `lazy import` declaration
    /// line, return the matching remap. Returns `None` for non-lazy
    /// import bindings (the common case).
    fn lazy_import_remap_for(&self, offset: usize) -> Option<&LazyImportRemap> {
        if self.lazy_import_remaps.is_empty() {
            return None;
        }
        let line = self.preprocessed_line_at_offset(offset);
        self.lazy_import_remaps
            .iter()
            .find(|r| r.line_index == line)
    }
}

/// Resolve a parsed module and return scopes + diagnostics. Uses the
/// default [`ResolveOptions`] (no `class!` tagging, etc.). Call
/// [`resolve_module_with`] from contexts that have preprocess metadata
/// available so the resolver can surface Typhon-specific markers on
/// the resulting bindings.
pub fn resolve_module(
    path: String,
    source: &str,
    module: &ModModule,
) -> (ResolvedModule, Diagnostics) {
    resolve_module_with(path, source, module, ResolveOptions::default())
}

/// Like [`resolve_module`] but with explicit options. Currently the only
/// option is `raw_class_byte_starts`, which lets the resolver tag every
/// `class!` binding with [`ClassKind::Raw`] so the LSP can render
/// distinct hover text and downstream passes can branch on the marker
/// without re-scanning byte ranges.
pub fn resolve_module_with(
    path: String,
    source: &str,
    module: &ModModule,
    options: ResolveOptions,
) -> (ResolvedModule, Diagnostics) {
    let mut r = Resolver::new_with(path, source, options);

    // First pass: collect top-level declarations so forward references
    // inside functions and classes resolve correctly.
    collect_top_level(&mut r, 0, &module.body);

    // Second pass: walk bodies to record references and inner scopes.
    for stmt in &module.body {
        walk_stmt(&mut r, 0, stmt);
    }

    r.report_unknown_names();
    r.report_unused_imports();
    r.report_main_not_called();

    let resolved = ResolvedModule {
        scopes: std::mem::take(&mut r.scopes),
        references: std::mem::take(&mut r.references),
    };
    let mut diagnostics = r.diagnostics;
    diagnostics.dedup();
    (resolved, diagnostics)
}

/// Search for `name` as a whole-word ASCII identifier in `source` starting
/// from `stmt_start`, after the keyword `keyword_prefix` (e.g. `"def "` or
/// `"class "`). Returns `(offset, end)` of the identifier, or a length-only
/// span at `stmt_start` if the pattern can't be found (shouldn't happen
/// for well-formed AST nodes).
fn find_def_name_span(
    source: &str,
    stmt_start: usize,
    keyword_prefix: &str,
    name: &str,
) -> (usize, usize) {
    if stmt_start >= source.len() {
        return (stmt_start, stmt_start + name.len());
    }
    let haystack = &source[stmt_start..];
    if let Some(rel) = haystack.find(keyword_prefix) {
        let mut cursor = stmt_start + rel + keyword_prefix.len();
        let bytes = source.as_bytes();
        while cursor < bytes.len() && (bytes[cursor] == b' ' || bytes[cursor] == b'\t') {
            cursor += 1;
        }
        if source[cursor..].starts_with(name) {
            return (cursor, cursor + name.len());
        }
    }
    (stmt_start, stmt_start + name.len())
}

/// Pre-declare names that should be visible across the whole body. Runs in
/// two sub-passes so that `val` values are registered *before* function /
/// class / import names — this lets the val-immutability check fire when a
/// later `def x` or `class x` collides with an earlier `let x`.
fn collect_top_level(r: &mut Resolver, scope: ScopeId, body: &[Stmt]) {
    // Sub-pass 0: harvest `global X` / `nonlocal X` declarations so the
    // missing_binding_kind check in `declare_target` sees them on the
    // pre-collect walk too (not just the second resolve pass).
    // FINDINGS #61.
    for stmt in body {
        match stmt {
            Stmt::Global(g) => {
                let entry = r.global_nonlocal_names.entry(scope).or_default();
                for ident in &g.names {
                    entry.insert(ident.id.as_str().to_owned());
                }
            }
            Stmt::Nonlocal(n) => {
                let entry = r.global_nonlocal_names.entry(scope).or_default();
                for ident in &n.names {
                    entry.insert(ident.id.as_str().to_owned());
                }
            }
            _ => {}
        }
    }
    // Sub-pass 1: value bindings (so val-protection sees them first).
    let default_val = r.scopes[scope].kind == ScopeKind::Module;
    for stmt in body {
        match stmt {
            Stmt::Assign(a) => {
                for t in &a.targets {
                    declare_target(r, scope, t, default_val, a.mutability);
                }
            }
            Stmt::AnnAssign(a) => {
                // FINDINGS #91: an annotated declaration without an
                // initialiser is a user error — skip the declaration
                // so the second pass can emit `tyc::missing_initialiser`
                // without also tripping `tyc::immutable_assign` on the
                // user's subsequent `x = <expr>` reassignment.
                if !(a.value.is_none() && a.mutability.is_some()) {
                    declare_target(r, scope, &a.target, default_val, a.mutability);
                }
            }
            _ => {}
        }
    }

    // Sub-pass 2: functions, classes, imports.
    for stmt in body {
        match stmt {
            Stmt::FunctionDef(f) => {
                let span = find_def_name_span(
                    r.source,
                    f.range.start().to_usize(),
                    "def ",
                    f.name.as_str(),
                );
                r.declare(
                    scope,
                    f.name.as_str(),
                    BindingKind::Function,
                    Mutability::Mut,
                    span,
                );
            }
            Stmt::ClassDef(c) => {
                let stmt_start = c.range.start().to_usize();
                let span = find_def_name_span(r.source, stmt_start, "class ", c.name.as_str());
                let kind = if r.is_raw_class_offset(stmt_start) {
                    ClassKind::Raw
                } else {
                    ClassKind::Plain
                };
                r.declare_class(scope, c.name.as_str(), span, kind);
            }
            Stmt::Import(i) => {
                for alias in &i.names {
                    // `import pkg.sub` binds the top-level name `pkg` in
                    // Python; only the explicit `as` form binds the dotted
                    // path under a new name.
                    let bound_name = match &alias.asname {
                        Some(as_name) => as_name.as_str().to_owned(),
                        None => alias
                            .name
                            .as_str()
                            .split('.')
                            .next()
                            .unwrap_or(alias.name.as_str())
                            .to_owned(),
                    };
                    let span = (
                        alias.range.start().to_usize(),
                        alias.range.start().to_usize() + bound_name.len(),
                    );
                    let module = if alias.asname.is_some() {
                        alias.name.as_str().to_owned()
                    } else {
                        // Bare `import pkg.sub` binds `pkg`; the import
                        // target is still the leaf-most module that name
                        // brings into scope — encode `pkg` so the LSP
                        // jumps to `pkg/__init__.ty`.
                        bound_name.clone()
                    };
                    r.declare_with(
                        scope,
                        &bound_name,
                        BindingKind::Import,
                        Mutability::Mut,
                        span,
                        Some(ImportInfo {
                            module,
                            member: None,
                        }),
                    );
                }
            }
            Stmt::ImportFrom(i) => {
                let module = i.module.as_ref().map(|m| m.as_str().to_owned());
                for alias in &i.names {
                    let name = alias.asname.as_ref().unwrap_or(&alias.name);
                    let span = (
                        alias.range.start().to_usize(),
                        alias.range.start().to_usize() + name.as_str().len(),
                    );
                    // Typhon-specific rejections for `from typing import X`:
                    //   - TypeVar: use PEP 695 `[T]` syntax instead (FINDINGS #73).
                    //   - List/Dict/Tuple/Set/FrozenSet/Type: use the
                    //     lowercase built-in form (FINDINGS #74).
                    if module.as_deref() == Some("typing") {
                        let imported = alias.name.as_str();
                        let imported_span = (
                            alias.range.start().to_usize(),
                            alias.range.start().to_usize() + imported.len(),
                        );
                        let length = imported_span.1.saturating_sub(imported_span.0).max(1);
                        if imported == "TypeVar" {
                            r.diagnostics.push_error(TycError::typevar_import_rejected(
                                &r.path,
                                r.source,
                                imported_span.0,
                                length,
                            ));
                        } else if let Some(lower) = lowercase_typing_alias(imported) {
                            r.diagnostics
                                .push_warning(TycError::typing_alias_deprecated(
                                    imported,
                                    lower,
                                    &r.path,
                                    r.source,
                                    imported_span.0,
                                    length,
                                ));
                        }
                    }
                    r.declare_with(
                        scope,
                        name.as_str(),
                        BindingKind::Import,
                        Mutability::Mut,
                        span,
                        module.as_ref().map(|m| ImportInfo {
                            module: m.clone(),
                            member: Some(alias.name.as_str().to_owned()),
                        }),
                    );
                }
            }
            // PEP 695 type alias — `type Vector[T] = list[T]`. The alias name
            // becomes a value-class binding in the enclosing scope.
            Stmt::TypeAlias(ta) => {
                if let Expr::Name(n) = ta.name.as_ref() {
                    let span = (
                        n.range.start().to_usize(),
                        n.range.start().to_usize() + n.id.as_str().len(),
                    );
                    r.declare(
                        scope,
                        n.id.as_str(),
                        BindingKind::Class,
                        Mutability::Let,
                        span,
                    );
                }
            }
            _ => {}
        }
    }
}

/// Slice `source` against a `TextRange`, returning `None` if the range
/// falls outside the source's byte bounds. Used to recover the surface
/// text of a node for inclusion in diagnostic messages.
fn source_slice(source: &str, range: TextRange) -> Option<&str> {
    let start = range.start().to_usize();
    let end = range.end().to_usize();
    if end > source.len() || start > end {
        return None;
    }
    source.get(start..end)
}

/// Map a capitalised `typing.<Name>` alias to its lowercase built-in
/// equivalent, when the built-in form exists. Returns `None` for typing
/// names that aren't a direct alias of a Python built-in (e.g. `Optional`,
/// `Union`, `Callable` — those have their own Typhon-native shapes).
fn lowercase_typing_alias(name: &str) -> Option<&'static str> {
    match name {
        "List" => Some("list"),
        "Dict" => Some("dict"),
        "Tuple" => Some("tuple"),
        "Set" => Some("set"),
        "FrozenSet" => Some("frozenset"),
        "Type" => Some("type"),
        _ => None,
    }
}

/// True when this assignment-target expression introduces one or more
/// names into scope (Name, Tuple-of-targets, List-of-targets, or a
/// Starred wrapper around one of those). Attribute / subscript targets
/// are *not* declarations — they're mutations of an existing object.
fn assignment_target_declares_names(t: &Expr) -> bool {
    matches!(
        t,
        Expr::Name(_) | Expr::Tuple(_) | Expr::List(_) | Expr::Starred(_)
    )
}

fn declare_target(
    r: &mut Resolver,
    scope: ScopeId,
    target: &Expr,
    default_val: bool,
    ast_mutability: Option<ast::Mutability>,
) {
    match target {
        Expr::Name(n) => {
            // When no explicit `let`/`mut` keyword is present in the AST,
            // treat a bare assignment as a rebinding of any existing binding
            // (taking its mutability) rather than a fresh declaration. Only
            // the *first* bareword assignment in a module scope defaults to
            // `let`; later bare assignments inherit the existing binding's
            // mutability.
            let existing_mut = r.lookup_local(scope, n.id.as_str()).map(|b| b.mutability);
            // Rule 2 of Typhon: a first bareword assignment to a new name
            // inside a function/method scope is `tyc::missing_binding_kind`.
            // Skipped for compiler-synthesised `__typhon_*` temporaries
            // (e.g. the `?` operator's `__typhon_q_N__`, `with`-chain
            // intermediates, auto-gather TaskGroup names) so desugar
            // bridges don't trigger user-facing errors.
            let span = (
                n.range.start().to_usize(),
                n.range.start().to_usize() + n.id.as_str().len(),
            );
            let declared_global_or_nonlocal = r
                .global_nonlocal_names
                .get(&scope)
                .is_some_and(|set| set.contains(n.id.as_str()));
            if ast_mutability.is_none()
                && existing_mut.is_none()
                && r.scopes[scope].kind == ScopeKind::Function
                && !n.id.as_str().starts_with("__typhon_")
                && !declared_global_or_nonlocal
                && r.seen_missing_binding_kind.insert((scope, span))
            {
                r.diagnostics.push_error(TycError::missing_binding_kind(
                    n.id.as_str(),
                    &r.path,
                    r.source,
                    span.0,
                    n.id.as_str().len().max(1),
                ));
            }
            // FINDINGS #76: when the user writes a fresh `let`/`mut`
            // binding for a name that was previously declared with an
            // explicit `let`/`mut` in this function scope, they almost
            // certainly mean block-scoped shadowing — which Python
            // doesn't support (names are function-scoped). Surface a
            // dedicated `tyc::no_block_shadow` with help that tells
            // the user to pick a different name. Otherwise the user
            // would get a generic `tyc::immutable_assign` whose
            // "change `let` to `mut`" suggestion is wrong for
            // shadowing intent.
            //
            // Only fire when the existing binding is also a value
            // declaration (`BindingKind::Value`). A parameter / loop
            // target / import / function / class re-declaration is a
            // separate problem and not what this finding is about.
            if ast_mutability.is_some() {
                if let Some(existing) = r.lookup_local(scope, n.id.as_str()) {
                    if existing.kind == BindingKind::Value {
                        let decl_span = existing.span;
                        if decl_span != span && r.seen_immutable_redecl.insert((decl_span, span)) {
                            r.diagnostics.push_error(TycError::no_block_shadow(
                                n.id.as_str(),
                                &r.path,
                                r.source,
                                decl_span.0,
                                decl_span.1.saturating_sub(decl_span.0).max(1),
                                span.0,
                                span.1.saturating_sub(span.0).max(1),
                            ));
                        }
                        return;
                    }
                }
            }
            let mutability = match ast_mutability {
                Some(ast::Mutability::Let) => Mutability::Let,
                Some(ast::Mutability::Mut) => Mutability::Mut,
                None => existing_mut.unwrap_or(if default_val {
                    Mutability::Let
                } else {
                    Mutability::Mut
                }),
            };
            r.declare(scope, n.id.as_str(), BindingKind::Value, mutability, span);
        }
        // Tuple destructuring (`a, b = expr`, `(a, b) = expr`) and list
        // destructuring (`[a, b] = expr`) recurse into each element. Used
        // by the best-effort `gather:` lowering, which produces a
        // `a, b = __typhon_gather_N__` assignment that previously left
        // `a` and `b` undeclared (FINDINGS #4).
        Expr::Tuple(t) => {
            for elt in &t.elts {
                declare_target(r, scope, elt, default_val, ast_mutability);
            }
        }
        Expr::List(l) => {
            for elt in &l.elts {
                declare_target(r, scope, elt, default_val, ast_mutability);
            }
        }
        Expr::Starred(s) => {
            declare_target(r, scope, &s.value, default_val, ast_mutability);
        }
        _ => {}
    }
}

/// Declare the target(s) bound by a `for` / `with` / `async for` statement.
///
/// Recurses through `Expr::Tuple`, `Expr::List`, and `Expr::Starred` so unpack
/// shapes (`for k, v in d.items():`, `for (a, b) in pairs:`, `for a, *rest in xs:`)
/// register every name they introduce as a `BindingKind::Loop` local. Without
/// this recursion the resolver only saw the outermost `Expr::Tuple` and the
/// names inside were treated as unknown — FINDINGS #40.
///
/// Loop / context-manager targets aren't subject to Rule 2 (the `for`/`with`
/// keyword itself introduces the binding), so this helper does not emit
/// `tyc::missing_binding_kind` like `declare_target` does for bare assignments.
///
/// Targets are declared as `Mutability::Let` (FINDINGS #75): the loop
/// itself rebinds the target each iteration through its own mechanism,
/// but a user-written `i = i + 1` inside the body is a Rule 2 violation
/// and now surfaces as `tyc::immutable_assign`.
fn declare_loop_target(r: &mut Resolver, scope: ScopeId, target: &Expr) {
    match target {
        Expr::Name(n) => {
            let span = (
                n.range.start().to_usize(),
                n.range.start().to_usize() + n.id.as_str().len(),
            );
            r.declare(
                scope,
                n.id.as_str(),
                BindingKind::Loop,
                Mutability::Let,
                span,
            );
        }
        Expr::Tuple(t) => {
            for elt in &t.elts {
                declare_loop_target(r, scope, elt);
            }
        }
        Expr::List(l) => {
            for elt in &l.elts {
                declare_loop_target(r, scope, elt);
            }
        }
        Expr::Starred(s) => {
            declare_loop_target(r, scope, &s.value);
        }
        _ => {}
    }
}

/// Walk a statement, recording references to names and descending into
/// nested function/class scopes.
/// Convert an AST `TextRange` to the (start, end) byte tuple used by
/// `Scope::span` and binding spans.
fn range_to_span(range: TextRange) -> (usize, usize) {
    (range.start().to_usize(), range.end().to_usize())
}

fn walk_stmt(r: &mut Resolver, scope: ScopeId, stmt: &Stmt) {
    match stmt {
        Stmt::FunctionDef(f) => {
            // Declare the function name in the enclosing scope.  Idempotent
            // at the same span — top-level defs are already pre-declared by
            // `collect_top_level`, but defs nested inside `with`, `if`, `try`,
            // etc. would otherwise leave the name unbound in the parent.
            let name_span = find_def_name_span(
                r.source,
                f.range.start().to_usize(),
                "def ",
                f.name.as_str(),
            );
            r.declare(
                scope,
                f.name.as_str(),
                BindingKind::Function,
                Mutability::Mut,
                name_span,
            );
            // Decorators are evaluated in the enclosing scope.
            for d in &f.decorator_list {
                walk_expr(r, scope, &d.expression);
            }
            let fn_scope = r.push_scope(ScopeKind::Function, scope, range_to_span(f.range));
            // PEP 695 type parameters (`def f[T](x: T) -> T`) bind into the
            // function scope so the parameter and return-type annotations can
            // resolve them.
            declare_type_params(r, fn_scope, f.type_params.as_deref());
            // Annotations on parameters/return type may reference the type
            // params, so resolve them in the function scope rather than the
            // enclosing one when type params are present.
            let ann_scope = if type_params_is_empty(f.type_params.as_deref()) {
                scope
            } else {
                fn_scope
            };
            walk_argument_annotations(r, ann_scope, &f.parameters);
            if let Some(ret) = &f.returns {
                walk_expr(r, ann_scope, ret);
            }
            // Parameters become bindings in the new scope.
            declare_arguments(r, fn_scope, &f.parameters);
            // Pre-collect declarations within the function body so forward
            // references work.
            collect_top_level(r, fn_scope, &f.body);
            for s in &f.body {
                walk_stmt(r, fn_scope, s);
            }
        }
        Stmt::ClassDef(c) => {
            // Declare the class name in the enclosing scope.  Same rationale
            // as the FunctionDef arm above — handles classes nested in `with`,
            // `if`, `try`, etc. which `collect_top_level` doesn't reach.
            let cls_stmt_start = c.range.start().to_usize();
            let cls_name_span =
                find_def_name_span(r.source, cls_stmt_start, "class ", c.name.as_str());
            let cls_kind = if r.is_raw_class_offset(cls_stmt_start) {
                ClassKind::Raw
            } else {
                ClassKind::Plain
            };
            r.declare_class(scope, c.name.as_str(), cls_name_span, cls_kind);
            for d in &c.decorator_list {
                walk_expr(r, scope, &d.expression);
            }
            let cls_scope = r.push_scope(ScopeKind::Class, scope, range_to_span(c.range));
            declare_type_params(r, cls_scope, c.type_params.as_deref());
            // Base classes that reference type params need the class scope.
            let base_scope = if type_params_is_empty(c.type_params.as_deref()) {
                scope
            } else {
                cls_scope
            };
            for base in c.bases() {
                walk_expr(r, base_scope, base);
            }
            collect_top_level(r, cls_scope, &c.body);
            let is_impl_stub = c.name.as_str().starts_with("__typhon_impl_");
            for s in &c.body {
                if is_impl_stub {
                    walk_impl_method(r, cls_scope, s);
                } else {
                    walk_stmt(r, cls_scope, s);
                }
            }
        }
        // `type Vector[T: float] = list[T]` — PEP 695 type alias statement.
        Stmt::TypeAlias(ta) => {
            // The type params and the value live in a synthetic alias scope so
            // the alias body can reference `T`. The alias name itself binds
            // into the enclosing scope and is already pre-declared by
            // `collect_top_level`.
            let alias_scope = r.push_scope(ScopeKind::Function, scope, range_to_span(ta.range));
            declare_type_params(r, alias_scope, ta.type_params.as_deref());
            walk_expr(r, alias_scope, &ta.value);
        }
        Stmt::Assign(a) => {
            walk_expr(r, scope, &a.value);
            let default_val = r.scopes[scope].kind == ScopeKind::Module;
            for t in &a.targets {
                // Names / Tuples / Lists / Starred are destructuring
                // targets handled by `declare_target`. Anything else
                // (e.g. `obj.attr = …`, `xs[0] = …`) is a reference, not
                // a binding declaration, so walk it for name resolution.
                if assignment_target_declares_names(t) {
                    declare_target(r, scope, t, default_val, a.mutability);
                } else {
                    walk_expr(r, scope, t);
                }
            }
        }
        Stmt::AnnAssign(a) => {
            if let Some(v) = &a.value {
                walk_expr(r, scope, v);
            }
            walk_expr(r, scope, &a.annotation);
            // FINDINGS #91: `let NAME: T` (or `mut NAME: T`) without an
            // initialiser produces a confusing immutable-assign error
            // when the user later writes `NAME = <expr>`. Reject the
            // declare-only form up front with a clear message that
            // tells the user to inline the initialiser. Bare AnnAssign
            // (no explicit `let`/`mut` keyword) is left alone because
            // class-body field declarations (`name: str`) and dataclass
            // attribute annotations legitimately omit initialisers.
            let missing_init = a.value.is_none() && a.mutability.is_some();
            if missing_init {
                if let Expr::Name(n) = a.target.as_ref() {
                    let keyword = match a.mutability {
                        Some(ast::Mutability::Let) => "let",
                        Some(ast::Mutability::Mut) => "mut",
                        None => unreachable!(),
                    };
                    let annotation = source_slice(r.source, a.annotation.range())
                        .unwrap_or("<type>")
                        .to_owned();
                    let span = (a.range.start().to_usize(), a.range.end().to_usize());
                    let length = span.1.saturating_sub(span.0).max(1);
                    r.diagnostics.push_error(TycError::missing_initialiser(
                        keyword,
                        n.id.as_str(),
                        annotation,
                        &r.path,
                        r.source,
                        span.0,
                        length,
                    ));
                }
            }
            if let Expr::Name(_) = a.target.as_ref() {
                let default_val = r.scopes[scope].kind == ScopeKind::Module;
                // When the initialiser is missing (FINDINGS #91), suppress
                // the declaration entirely. Otherwise a later `x = 5`
                // would either fire a redundant `tyc::immutable_assign`
                // (the misleading cascade the finding is about) or — for
                // `mut` — pretend the assignment was the first
                // initialiser, masking the original mistake. The
                // missing_initialiser error already tells the user what
                // to do; downstream references to the un-declared name
                // (`x` on subsequent lines) will produce the standard
                // unknown-name flow.
                if !missing_init {
                    declare_target(r, scope, &a.target, default_val, a.mutability);
                }
            }
        }
        Stmt::AugAssign(a) => {
            walk_expr(r, scope, &a.target);
            walk_expr(r, scope, &a.value);
        }
        Stmt::Return(ret) => {
            if let Some(v) = &ret.value {
                walk_expr(r, scope, v);
            }
        }
        Stmt::Expr(e) => walk_expr(r, scope, &e.value),
        Stmt::If(i) => {
            walk_expr(r, scope, &i.test);
            for s in &i.body {
                walk_stmt(r, scope, s);
            }
            for clause in &i.elif_else_clauses {
                if let Some(test) = &clause.test {
                    walk_expr(r, scope, test);
                }
                for s in &clause.body {
                    walk_stmt(r, scope, s);
                }
            }
        }
        Stmt::While(w) => {
            walk_expr(r, scope, &w.test);
            for s in &w.body {
                walk_stmt(r, scope, s);
            }
            for s in &w.orelse {
                walk_stmt(r, scope, s);
            }
        }
        Stmt::For(f) => {
            walk_expr(r, scope, &f.iter);
            // Loop target introduces one or more bindings. Recurse into
            // `Expr::Tuple` / `Expr::List` / `Expr::Starred` so unpack
            // forms (`for k, v in d.items():`, `for i, x in enumerate(xs):`,
            // `for (a, *rest) in pairs:`) declare every name they bind.
            declare_loop_target(r, scope, f.target.as_ref());
            for s in &f.body {
                walk_stmt(r, scope, s);
            }
            for s in &f.orelse {
                walk_stmt(r, scope, s);
            }
        }
        Stmt::With(w) => {
            for item in &w.items {
                walk_expr(r, scope, &item.context_expr);
                if let Some(var) = &item.optional_vars {
                    declare_loop_target(r, scope, var.as_ref());
                }
            }
            for s in &w.body {
                walk_stmt(r, scope, s);
            }
        }
        Stmt::Try(t) => {
            for s in &t.body {
                walk_stmt(r, scope, s);
            }
            for h in &t.handlers {
                let ast::ExceptHandler::ExceptHandler(h) = h;
                if let Some(typ) = &h.type_ {
                    walk_expr(r, scope, typ);
                }
                if let Some(name) = &h.name {
                    let span = (
                        h.range.start().to_usize(),
                        h.range.start().to_usize() + name.as_str().len(),
                    );
                    r.declare(
                        scope,
                        name.as_str(),
                        BindingKind::Loop,
                        Mutability::Mut,
                        span,
                    );
                }
                for s in &h.body {
                    walk_stmt(r, scope, s);
                }
            }
            for s in &t.orelse {
                walk_stmt(r, scope, s);
            }
            for s in &t.finalbody {
                walk_stmt(r, scope, s);
            }
        }
        Stmt::Raise(rs) => {
            if let Some(exc) = &rs.exc {
                walk_expr(r, scope, exc);
            }
            if let Some(cause) = &rs.cause {
                walk_expr(r, scope, cause);
            }
        }
        Stmt::Import(_) | Stmt::ImportFrom(_) => {
            // Already declared in collect_top_level.
        }
        Stmt::Pass(_) | Stmt::Break(_) | Stmt::Continue(_) => {}
        Stmt::Global(g) => {
            let entry = r.global_nonlocal_names.entry(scope).or_default();
            for ident in &g.names {
                entry.insert(ident.id.as_str().to_owned());
            }
        }
        Stmt::Nonlocal(n) => {
            let entry = r.global_nonlocal_names.entry(scope).or_default();
            for ident in &n.names {
                entry.insert(ident.id.as_str().to_owned());
            }
        }
        Stmt::Assert(a) => {
            walk_expr(r, scope, &a.test);
            if let Some(m) = &a.msg {
                walk_expr(r, scope, m);
            }
        }
        Stmt::Delete(d) => {
            for t in &d.targets {
                walk_expr(r, scope, t);
            }
        }
        Stmt::Match(m) => {
            walk_expr(r, scope, &m.subject);
            for case in &m.cases {
                walk_pattern(r, scope, &case.pattern);
                if let Some(g) = &case.guard {
                    walk_expr(r, scope, g);
                }
                for s in &case.body {
                    walk_stmt(r, scope, s);
                }
            }
        }
        _ => {}
    }
}

/// Walk a `match` pattern, recording name references (e.g. `Ok` in `case
/// Ok(value):`) and declaring name bindings (e.g. `value` in the same case).
///
/// Python semantics: case bindings are introduced in the enclosing scope and
/// remain visible after the `match` ends; they are rebindable like `for`
/// loop targets, so they are declared with [`Mutability::Mut`].
fn walk_pattern(r: &mut Resolver, scope: ScopeId, pattern: &ast::Pattern) {
    use ast::Pattern;
    match pattern {
        Pattern::MatchValue(p) => walk_expr(r, scope, &p.value),
        Pattern::MatchSingleton(_) => {}
        Pattern::MatchSequence(p) => {
            for sub in &p.patterns {
                walk_pattern(r, scope, sub);
            }
        }
        Pattern::MatchMapping(p) => {
            for k in &p.keys {
                walk_expr(r, scope, k);
            }
            for sub in &p.patterns {
                walk_pattern(r, scope, sub);
            }
            if let Some(rest) = &p.rest {
                let span = (
                    rest.range.start().to_usize(),
                    rest.range.start().to_usize() + rest.id.as_str().len(),
                );
                r.declare(
                    scope,
                    rest.id.as_str(),
                    BindingKind::Loop,
                    Mutability::Mut,
                    span,
                );
            }
        }
        Pattern::MatchClass(p) => {
            walk_expr(r, scope, &p.cls);
            for sub in &p.arguments.patterns {
                walk_pattern(r, scope, sub);
            }
            for kw in &p.arguments.keywords {
                walk_pattern(r, scope, &kw.pattern);
            }
        }
        Pattern::MatchStar(p) => {
            if let Some(name) = &p.name {
                let span = (
                    name.range.start().to_usize(),
                    name.range.start().to_usize() + name.id.as_str().len(),
                );
                r.declare(
                    scope,
                    name.id.as_str(),
                    BindingKind::Loop,
                    Mutability::Mut,
                    span,
                );
            }
        }
        Pattern::MatchAs(p) => {
            if let Some(sub) = &p.pattern {
                walk_pattern(r, scope, sub);
            }
            if let Some(name) = &p.name {
                let span = (
                    name.range.start().to_usize(),
                    name.range.start().to_usize() + name.id.as_str().len(),
                );
                r.declare(
                    scope,
                    name.id.as_str(),
                    BindingKind::Loop,
                    Mutability::Mut,
                    span,
                );
            }
        }
        Pattern::MatchOr(p) => {
            for sub in &p.patterns {
                walk_pattern(r, scope, sub);
            }
        }
    }
}

/// Walk every annotation expression on the parameters of a function, so
/// names used in those annotations are recorded as references and bound
/// against the enclosing scope.
///
/// Default values on parameters are also walked so names referenced there
/// (e.g. `Depends(get_db)` on a FastAPI dependency-injected parameter) are
/// recorded as references against the enclosing scope.
fn walk_argument_annotations(r: &mut Resolver, scope: ScopeId, args: &ast::Parameters) {
    let all = args
        .posonlyargs
        .iter()
        .chain(args.args.iter())
        .chain(args.kwonlyargs.iter());
    for arg in all {
        if let Some(ann) = &arg.parameter.annotation {
            walk_expr(r, scope, ann);
        }
        if let Some(def) = &arg.default {
            walk_expr(r, scope, def);
        }
    }
    if let Some(va) = &args.vararg {
        if let Some(ann) = &va.annotation {
            walk_expr(r, scope, ann);
        }
    }
    if let Some(kw) = &args.kwarg {
        if let Some(ann) = &kw.annotation {
            walk_expr(r, scope, ann);
        }
    }
}

/// Walk a single statement from an `impl` pseudo-class body.
///
/// Identical to [`walk_stmt`] for `FunctionDef` (sync and async), but
/// additionally pre-declares a synthetic `self` binding in each method's
/// scope.  The desugar pass injects `self` as the actual first parameter
/// later; this declaration prevents false "unknown name: self" errors during
/// resolution.  All other statement kinds fall through to [`walk_stmt`].
fn walk_impl_method(r: &mut Resolver, cls_scope: ScopeId, stmt: &Stmt) {
    match stmt {
        Stmt::FunctionDef(f) => {
            for d in &f.decorator_list {
                walk_expr(r, cls_scope, &d.expression);
            }
            let fn_scope = r.push_scope(ScopeKind::Function, cls_scope, range_to_span(f.range));
            // Pre-declare the implicit `self` the desugar pass will inject.
            r.declare(
                fn_scope,
                "self",
                BindingKind::Parameter,
                Mutability::Mut,
                (0, 0),
            );
            // PEP 695 method-level type parameters (`def map[U](...) ->
            // Box[U]:`) bind into the function scope so the parameter and
            // return-type annotations can resolve them. Without this the
            // resolver walks the annotations in `cls_scope`, where `U` is
            // unknown — the symptom for the docs' `Box[T].map[U]`
            // example. FINDINGS #59.
            declare_type_params(r, fn_scope, f.type_params.as_deref());
            let ann_scope = if type_params_is_empty(f.type_params.as_deref()) {
                cls_scope
            } else {
                fn_scope
            };
            walk_argument_annotations(r, ann_scope, &f.parameters);
            if let Some(ret) = &f.returns {
                walk_expr(r, ann_scope, ret);
            }
            declare_arguments(r, fn_scope, &f.parameters);
            collect_top_level(r, fn_scope, &f.body);
            for s in &f.body {
                walk_stmt(r, fn_scope, s);
            }
        }
        other => walk_stmt(r, cls_scope, other),
    }
}

/// Declare every PEP 695 type parameter (e.g. `T`, `U: Number`, `*Ts`,
/// `**P`) into `scope` so that annotations on parameters / bases / return
/// types resolve them as known names rather than reporting "unknown name".
///
/// Bounds (`T: Number`) are resolved in the enclosing scope where the bound
/// itself was written; we don't model variance / constraints in v1.
fn declare_type_params(r: &mut Resolver, scope: ScopeId, type_params: Option<&ast::TypeParams>) {
    let Some(tps) = type_params else { return };
    for tp in &tps.type_params {
        let (name, range, bound) = match tp {
            ast::TypeParam::TypeVar(t) => (t.name.as_str(), t.range, t.bound.as_deref()),
            ast::TypeParam::ParamSpec(p) => (p.name.as_str(), p.range, None),
            ast::TypeParam::TypeVarTuple(t) => (t.name.as_str(), t.range, None),
        };
        if let Some(b) = bound {
            walk_expr(r, scope, b);
        }
        let span = (
            range.start().to_usize(),
            range.start().to_usize() + name.len(),
        );
        r.declare(scope, name, BindingKind::Value, Mutability::Let, span);
    }
}

/// True when the function/class has no type parameters (either `None` or an
/// empty `TypeParams` list).
fn type_params_is_empty(type_params: Option<&ast::TypeParams>) -> bool {
    type_params.is_none_or(|t| t.type_params.is_empty())
}

fn declare_arguments(r: &mut Resolver, scope: ScopeId, args: &ast::Parameters) {
    let all = args
        .posonlyargs
        .iter()
        .chain(args.args.iter())
        .chain(args.kwonlyargs.iter());
    for arg in all {
        let span = (
            arg.parameter.range.start().to_usize(),
            arg.parameter.range.start().to_usize() + arg.parameter.name.as_str().len(),
        );
        r.declare(
            scope,
            arg.parameter.name.as_str(),
            BindingKind::Parameter,
            Mutability::Mut,
            span,
        );
    }
    if let Some(va) = &args.vararg {
        let span = (
            va.range.start().to_usize(),
            va.range.start().to_usize() + va.name.as_str().len(),
        );
        r.declare(
            scope,
            va.name.as_str(),
            BindingKind::Parameter,
            Mutability::Mut,
            span,
        );
    }
    if let Some(kw) = &args.kwarg {
        let span = (
            kw.range.start().to_usize(),
            kw.range.start().to_usize() + kw.name.as_str().len(),
        );
        r.declare(
            scope,
            kw.name.as_str(),
            BindingKind::Parameter,
            Mutability::Mut,
            span,
        );
    }
}

/// Walk an expression, recording every name reference.
fn walk_expr(r: &mut Resolver, scope: ScopeId, expr: &Expr) {
    match expr {
        Expr::Name(n) => {
            let span = (
                n.range.start().to_usize(),
                n.range.start().to_usize() + n.id.as_str().len(),
            );
            r.reference(scope, n.id.as_str(), span);
        }
        Expr::BinOp(b) => {
            walk_expr(r, scope, &b.left);
            walk_expr(r, scope, &b.right);
        }
        Expr::BoolOp(b) => {
            for v in &b.values {
                walk_expr(r, scope, v);
            }
        }
        Expr::UnaryOp(u) => walk_expr(r, scope, &u.operand),
        Expr::Call(c) => {
            walk_expr(r, scope, &c.func);
            for a in &c.arguments.args {
                walk_expr(r, scope, a);
            }
            for k in &c.arguments.keywords {
                walk_expr(r, scope, &k.value);
            }
        }
        Expr::Attribute(a) => walk_expr(r, scope, &a.value),
        Expr::Subscript(s) => {
            walk_expr(r, scope, &s.value);
            walk_expr(r, scope, &s.slice);
        }
        Expr::Compare(c) => {
            walk_expr(r, scope, &c.left);
            for c2 in c.comparators.iter() {
                walk_expr(r, scope, c2);
            }
        }
        Expr::Tuple(t) => {
            for e in &t.elts {
                walk_expr(r, scope, e);
            }
        }
        Expr::List(l) => {
            for e in &l.elts {
                walk_expr(r, scope, e);
            }
        }
        Expr::Set(s) => {
            for e in &s.elts {
                walk_expr(r, scope, e);
            }
        }
        Expr::Dict(d) => {
            for item in &d.items {
                if let Some(k) = &item.key {
                    walk_expr(r, scope, k);
                }
                walk_expr(r, scope, &item.value);
            }
        }
        Expr::If(i) => {
            walk_expr(r, scope, &i.test);
            walk_expr(r, scope, &i.body);
            walk_expr(r, scope, &i.orelse);
        }
        Expr::Slice(s) => {
            if let Some(lo) = &s.lower {
                walk_expr(r, scope, lo);
            }
            if let Some(hi) = &s.upper {
                walk_expr(r, scope, hi);
            }
            if let Some(st) = &s.step {
                walk_expr(r, scope, st);
            }
        }
        Expr::Starred(s) => walk_expr(r, scope, &s.value),
        Expr::Await(a) => walk_expr(r, scope, &a.value),
        Expr::Yield(y) => {
            if let Some(v) = &y.value {
                walk_expr(r, scope, v);
            }
        }
        Expr::YieldFrom(y) => walk_expr(r, scope, &y.value),
        Expr::Lambda(l) => {
            let scope2 = r.push_scope(ScopeKind::Function, scope, range_to_span(l.range));
            if let Some(params) = &l.parameters {
                declare_arguments(r, scope2, params);
            }
            walk_expr(r, scope2, &l.body);
        }
        Expr::ListComp(c) => walk_comp(r, scope, range_to_span(c.range), &c.elt, &c.generators),
        Expr::SetComp(c) => walk_comp(r, scope, range_to_span(c.range), &c.elt, &c.generators),
        Expr::Generator(g) => walk_comp(r, scope, range_to_span(g.range), &g.elt, &g.generators),
        Expr::DictComp(c) => {
            let scope2 = r.push_scope(ScopeKind::Comprehension, scope, range_to_span(c.range));
            for gen in &c.generators {
                walk_expr(r, scope2, &gen.iter);
                if let Expr::Name(n) = &gen.target {
                    let span = (
                        n.range.start().to_usize(),
                        n.range.start().to_usize() + n.id.as_str().len(),
                    );
                    r.declare(
                        scope2,
                        n.id.as_str(),
                        BindingKind::Loop,
                        Mutability::Mut,
                        span,
                    );
                }
                for cond in &gen.ifs {
                    walk_expr(r, scope2, cond);
                }
            }
            if let Some(key) = &c.key {
                walk_expr(r, scope2, key);
            }
            walk_expr(r, scope2, &c.value);
        }
        // Literal-shaped expressions with no embedded references.
        Expr::NumberLiteral(_)
        | Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NoneLiteral(_)
        | Expr::EllipsisLiteral(_)
        | Expr::IpyEscapeCommand(_) => {}
        // f-strings and t-strings carry interpolated expressions inside
        // their `value` structure (ruff folds the rustpython
        // `FormattedValue`/`JoinedStr` variants away). Walk every
        // interpolation so name references inside `f"{x}"` still feed
        // unknown-name and unused-binding diagnostics. Format-specs are
        // themselves InterpolatedStringElements, so a nested `{spec}` is
        // visited via the same path on the next pass through this code.
        Expr::FString(fs) => {
            for elem in fs.value.elements() {
                if let ast::InterpolatedStringElement::Interpolation(interp) = elem {
                    walk_expr(r, scope, &interp.expression);
                }
            }
        }
        Expr::TString(ts) => {
            for elem in ts.value.elements() {
                if let ast::InterpolatedStringElement::Interpolation(interp) = elem {
                    walk_expr(r, scope, &interp.expression);
                }
            }
        }
        Expr::Named(n) => {
            walk_expr(r, scope, &n.value);
            if let Expr::Name(name) = n.target.as_ref() {
                let span = (
                    name.range.start().to_usize(),
                    name.range.start().to_usize() + name.id.as_str().len(),
                );
                r.declare(
                    scope,
                    name.id.as_str(),
                    BindingKind::Value,
                    Mutability::Mut,
                    span,
                );
            }
        }
    }
}

fn walk_comp(
    r: &mut Resolver,
    scope: ScopeId,
    span: (usize, usize),
    elt: &Expr,
    generators: &[ast::Comprehension],
) {
    let scope2 = r.push_scope(ScopeKind::Comprehension, scope, span);
    for gen in generators {
        walk_expr(r, scope2, &gen.iter);
        // Comprehension targets share the recursive shape of `for`/`with`
        // targets — e.g. `[v for k, v in d.items()]` binds both `k` and `v`.
        declare_loop_target(r, scope2, &gen.target);
        for cond in &gen.ifs {
            walk_expr(r, scope2, cond);
        }
    }
    walk_expr(r, scope2, elt);
}

/// A conservative list of Python built-in names that the resolver treats
/// as always-in-scope. Not exhaustive — the goal is to avoid false-positive
/// "unknown name" diagnostics for common identifiers in Phase 1.
fn builtin_names() -> std::collections::HashSet<&'static str> {
    let names: &[&'static str] = &[
        // Built-in functions
        "print",
        "len",
        "range",
        "abs",
        "min",
        "max",
        "sum",
        "any",
        "all",
        "sorted",
        "reversed",
        "enumerate",
        "zip",
        "map",
        "filter",
        "isinstance",
        "issubclass",
        "hasattr",
        "getattr",
        "setattr",
        "delattr",
        "iter",
        "next",
        "repr",
        "id",
        "hash",
        "type",
        "vars",
        "dir",
        "callable",
        "input",
        "open",
        "exit",
        "quit",
        "breakpoint",
        "format",
        "ord",
        "chr",
        "hex",
        "oct",
        "bin",
        "round",
        "pow",
        "divmod",
        "globals",
        "locals",
        "eval",
        "exec",
        "compile",
        "object",
        "super",
        "property",
        "classmethod",
        "staticmethod",
        "frozenset",
        // Built-in types
        "int",
        "str",
        "bool",
        "float",
        "complex",
        "bytes",
        "bytearray",
        "memoryview",
        "list",
        "tuple",
        "set",
        "dict",
        "type",
        // Constants
        "True",
        "False",
        "None",
        "Ellipsis",
        "NotImplemented",
        "__name__",
        "__file__",
        "__doc__",
        "__builtins__",
        "__package__",
        "__loader__",
        "__spec__",
        "__debug__",
        // Common exceptions
        "Exception",
        "BaseException",
        "ValueError",
        "TypeError",
        "KeyError",
        "IndexError",
        "AttributeError",
        "RuntimeError",
        "StopIteration",
        "StopAsyncIteration",
        "GeneratorExit",
        "FileNotFoundError",
        "FileExistsError",
        "PermissionError",
        "NotImplementedError",
        "ZeroDivisionError",
        "OverflowError",
        "ArithmeticError",
        "OSError",
        "IOError",
        "ImportError",
        "ModuleNotFoundError",
        "LookupError",
        "NameError",
        "UnicodeError",
        "UnicodeDecodeError",
        "UnicodeEncodeError",
        "AssertionError",
        "SyntaxError",
        "IndentationError",
        "TabError",
        "SystemError",
        "SystemExit",
        "KeyboardInterrupt",
        "MemoryError",
        "RecursionError",
        // Phase-1 typing names commonly used in annotations
        "Optional",
        "Union",
        "Any",
        "Callable",
        "Iterable",
        "Iterator",
        "Sequence",
        "Mapping",
        "MutableMapping",
        "List",
        "Dict",
        "Set",
        "Tuple",
        "FrozenSet",
        "Type",
        "TypeVar",
        "Generic",
        "Protocol",
        "Self",
        "ClassVar",
        "Final",
        "Literal",
        "NoReturn",
        "Awaitable",
        "Coroutine",
        "Generator",
        "AsyncIterator",
        "AsyncIterable",
        // Typhon Result type constructors (from typhon_runtime).
        "Ok",
        "Err",
        "Result",
        // Typhon comptime built-in function.
        "env",
        // Pydantic BaseModel — injected by the `model` keyword preprocessor.
        "BaseModel",
        // Pydantic ConfigDict — used by the `model` desugar injection.
        "ConfigDict",
        // Generated by the Phase 3 `gather` and `go` lowerings; the desugar
        // pass inserts `import asyncio` / `import typhon_runtime` itself, but
        // the resolver still sees references before that injection runs.
        "asyncio",
        "typhon_runtime",
        // Decorators that may appear without an import in user code.
        "pure",
        "memo",
        "gatherable",
        "runtime_checkable",
        "functools",
        "dataclass",
        "dataclasses",
    ];
    names.iter().copied().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tyc_syntax::preprocess::preprocess;

    fn resolve(src: &str) -> (ResolvedModule, Diagnostics) {
        resolve_with_options(src, ResolveOptions::default())
    }

    fn resolve_with_options(src: &str, options: ResolveOptions) -> (ResolvedModule, Diagnostics) {
        let prep = preprocess(src);
        let module = tyc_syntax::parse_module(&prep.python_source)
            .unwrap()
            .into_syntax();
        resolve_module_with("<test>".to_owned(), &prep.python_source, &module, options)
    }

    fn resolve_with_raw_classes(src: &str) -> (ResolvedModule, Diagnostics) {
        let prep = preprocess(src);
        let raw_class_byte_starts =
            tyc_syntax::preprocess::line_byte_starts(&prep.python_source, &prep.raw_class_lines);
        let options = ResolveOptions {
            raw_class_byte_starts,
            ..ResolveOptions::default()
        };
        let module = tyc_syntax::parse_module(&prep.python_source)
            .unwrap()
            .into_syntax();
        resolve_module_with("<test>".to_owned(), &prep.python_source, &module, options)
    }

    #[test]
    fn raw_class_binding_carries_raw_class_kind() {
        let src = "class! Foo:\n    pass\n";
        let (m, _) = resolve_with_raw_classes(src);
        let b = m
            .module_scope()
            .lookup_local("Foo")
            .expect("Foo must be declared");
        assert_eq!(b.kind, BindingKind::Class);
        assert_eq!(b.class_kind, ClassKind::Raw);
    }

    #[test]
    fn plain_class_binding_keeps_plain_class_kind() {
        let src = "class Foo:\n    pass\n";
        let (m, _) = resolve_with_raw_classes(src);
        let b = m
            .module_scope()
            .lookup_local("Foo")
            .expect("Foo must be declared");
        assert_eq!(b.class_kind, ClassKind::Plain);
    }

    #[test]
    fn raw_class_tagging_is_offset_specific() {
        // Two classes, only the second is raw. The tagging must pick out
        // exactly the right binding.
        let src = "class A:\n    pass\nclass! B:\n    pass\n";
        let (m, _) = resolve_with_raw_classes(src);
        let a = m.module_scope().lookup_local("A").unwrap();
        let b = m.module_scope().lookup_local("B").unwrap();
        assert_eq!(a.class_kind, ClassKind::Plain);
        assert_eq!(b.class_kind, ClassKind::Raw);
    }

    #[test]
    fn resolve_with_default_options_never_tags_raw() {
        // Without the byte-starts list, no class is tagged raw even if the
        // source uses `class!` — keeps the default API safe for callers
        // that don't have preprocess metadata handy.
        let src = "class! Foo:\n    pass\n";
        let (m, _) = resolve(src);
        let b = m.module_scope().lookup_local("Foo").unwrap();
        assert_eq!(b.class_kind, ClassKind::Plain);
    }
    // Silence the unused-helper warning when the doc tests are skipped.
    #[allow(dead_code)]
    fn _resolve_with_options_must_compile(src: &str) {
        let _ = resolve_with_options(src, ResolveOptions::default());
    }

    #[test]
    fn scope_at_offset_picks_innermost_function() {
        // The cursor sits inside `inner`'s body; `scope_at_offset` should
        // return that function's scope, not the enclosing `outer` or module.
        let src = "\
def outer(a):
    def inner(b):
        return a + b
";
        let (m, _) = resolve(src);
        // `b` first appears on the `return a + b` line; pick a byte offset
        // inside that line.
        let needle = "return a + b";
        let offset = src.find(needle).unwrap();
        let id = m.scope_at_offset(offset);
        assert_eq!(m.scopes[id].kind, ScopeKind::Function);
        // The chosen scope should contain a binding for `b` (inner's param)
        // but not for `outer`'s `a` directly — `a` is reached via parent.
        assert!(m.scopes[id].lookup_local("b").is_some());
        assert!(m.scopes[id].lookup_local("a").is_none());
    }

    #[test]
    fn visible_bindings_walks_parent_chain() {
        let src = "\
def outer(a):
    def inner(b):
        return a + b
";
        let (m, _) = resolve(src);
        let needle = "return a + b";
        let offset = src.find(needle).unwrap();
        let id = m.scope_at_offset(offset);
        let names: Vec<String> = m
            .visible_bindings(id)
            .into_iter()
            .map(|b| b.name.clone())
            .collect();
        assert!(names.contains(&"a".to_owned()), "expected a in {names:?}");
        assert!(names.contains(&"b".to_owned()), "expected b in {names:?}");
        assert!(
            names.contains(&"outer".to_owned()),
            "expected outer in {names:?}"
        );
        assert!(
            names.contains(&"inner".to_owned()),
            "expected inner in {names:?}"
        );
    }

    #[test]
    fn scope_at_offset_outside_function_returns_module() {
        let src = "\
let x: int = 1
def foo():
    return x
";
        let (m, _) = resolve(src);
        // Offset 0: very top of file, before any function definition.
        let id = m.scope_at_offset(0);
        assert_eq!(m.scopes[id].kind, ScopeKind::Module);
    }

    #[test]
    fn collects_let_binding() {
        let (m, d) = resolve("let x: int = 1\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
        let scope = m.module_scope();
        let x = scope.lookup_local("x").unwrap();
        assert_eq!(x.mutability, Mutability::Let);
    }

    #[test]
    fn collects_mut_binding() {
        let (m, d) = resolve("mut count: int = 0\n");
        assert!(!d.has_errors());
        let count = m.module_scope().lookup_local("count").unwrap();
        assert_eq!(count.mutability, Mutability::Mut);
    }

    #[test]
    fn val_reassignment_is_an_error() {
        let (_m, d) = resolve("let x: int = 1\nx = 2\n");
        assert!(d.has_errors());
        let msg = format!("{}", d.errors()[0]);
        assert!(
            msg.contains("cannot assign to immutable binding 'x'"),
            "got {}",
            msg
        );
    }

    #[test]
    fn mut_reassignment_is_ok() {
        let (_m, d) = resolve("mut x: int = 1\nx = 2\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn duplicate_let_emits_one_diagnostic() {
        // The resolver double-visits each body (pre-collect + walk_stmt);
        // a re-declaration must only be reported once. Since FINDINGS
        // #76, `let x = 1; let x = 2` surfaces `tyc::no_block_shadow`
        // (a clearer diagnostic for the shadowing case) instead of the
        // generic `tyc::immutable_assign`.
        let (_m, d) = resolve("let x = 1\nlet x = 2\n");
        let shadow_errors: Vec<_> = d
            .errors()
            .iter()
            .filter(|e| matches!(e, TycError::NoBlockShadow { .. }))
            .collect();
        assert_eq!(
            shadow_errors.len(),
            1,
            "expected exactly one no_block_shadow diagnostic, got {}: {:?}",
            shadow_errors.len(),
            d.errors()
        );
    }

    #[test]
    fn block_let_shadow_uses_dedicated_diagnostic() {
        // FINDINGS #76: a `let` declaration inside an `if`/`while`/`for`
        // block that names an outer binding is shadowing intent, not a
        // re-assignment. Surface `tyc::no_block_shadow` so the help text
        // can explain Python's function-level scoping.
        let src = "def main() -> None:\n\
                   \x20   let x: int = 1\n\
                   \x20   if True:\n\
                   \x20       let x: str = \"hi\"\n\
                   \x20       print(x)\n";
        let (_m, d) = resolve(src);
        assert!(
            d.errors()
                .iter()
                .any(|e| matches!(e, TycError::NoBlockShadow { name, .. } if name == "x")),
            "expected NoBlockShadow on inner `let x`; got {:?}",
            d.errors()
        );
        // The generic immutable-assign diagnostic should NOT also fire —
        // the no-block-shadow path returns early after recording the
        // dedicated error.
        assert!(
            !d.errors()
                .iter()
                .any(|e| matches!(e, TycError::ImmutableAssign { .. })),
            "shadowing must not also fire immutable_assign: {:?}",
            d.errors()
        );
    }

    #[test]
    fn unknown_name_errors() {
        let (_m, d) = resolve("y = z + 1\n");
        assert!(d.has_errors());
        let msg = format!("{}", d.errors()[0]);
        assert!(msg.contains("cannot find 'z'"), "got {}", msg);
    }

    #[test]
    fn main_defined_but_not_called_warns() {
        // FINDINGS #92: a top-level `def main()` with no call site
        // should produce the `tyc::main_not_called` advice diagnostic
        // (stored as a warning).
        let (_m, d) = resolve("def main() -> None:\n    print(\"hi\")\n");
        assert!(
            d.warnings()
                .iter()
                .any(|w| matches!(w, TycError::MainNotCalled { .. })),
            "expected MainNotCalled warning; got {:?}",
            d.warnings()
        );
    }

    #[test]
    fn main_called_at_module_level_is_clean() {
        let src = "def main() -> None:\n\
                   \x20   print(\"hi\")\n\
                   \n\
                   if __name__ == \"__main__\":\n\
                   \x20   main()\n";
        let (_m, d) = resolve(src);
        assert!(
            !d.warnings()
                .iter()
                .any(|w| matches!(w, TycError::MainNotCalled { .. })),
            "main() in __name__ block must suppress the diagnostic: {:?}",
            d.warnings()
        );
    }

    #[test]
    fn module_with_all_export_suppresses_main_not_called() {
        // Library shape: `__all__` lists exported names. A `main`
        // declared for export shouldn't trigger the advice.
        let src = "__all__ = [\"main\"]\n\
                   def main() -> None:\n\
                   \x20   print(\"hi\")\n";
        let (_m, d) = resolve(src);
        assert!(
            !d.warnings()
                .iter()
                .any(|w| matches!(w, TycError::MainNotCalled { .. })),
            "module with __all__ must suppress the diagnostic: {:?}",
            d.warnings()
        );
    }

    fn parse_module(src: &str) -> ruff_python_ast::ModModule {
        tyc_syntax::parse_module(src).unwrap().into_syntax()
    }

    #[test]
    fn unknown_module_warns_when_root_not_resolvable() {
        // FINDINGS #79: `from other import helper` where `other` is
        // neither in stdlib, the project, nor a declared dep.
        let module = parse_module("from other import helper\n");
        let diags = check_unknown_modules("t.ty", "from other import helper\n", &module, &[], &[]);
        assert!(
            diags
                .warnings()
                .iter()
                .any(|e| matches!(e, TycError::UnknownModule { module, .. } if module == "other")),
            "expected UnknownModule warning; got {:?}",
            diags.warnings()
        );
    }

    #[test]
    fn stdlib_module_is_clean() {
        let module = parse_module("import os\nfrom collections import defaultdict\n");
        let diags = check_unknown_modules(
            "t.ty",
            "import os\nfrom collections import defaultdict\n",
            &module,
            &[],
            &[],
        );
        assert!(
            diags.warnings().is_empty(),
            "stdlib modules must not warn; got {:?}",
            diags.warnings()
        );
    }

    #[test]
    fn project_module_is_clean() {
        // A `from pkg.sub import foo` is OK when "pkg" appears in the
        // project module list (the root is what we vet).
        let module = parse_module("from pkg.sub import foo\n");
        let diags = check_unknown_modules(
            "t.ty",
            "from pkg.sub import foo\n",
            &module,
            &["pkg.sub".to_string()],
            &[],
        );
        assert!(
            diags.warnings().is_empty(),
            "project module must not warn; got {:?}",
            diags.warnings()
        );
    }

    #[test]
    fn typhon_runtime_is_clean() {
        let module = parse_module("from typhon_runtime.lazy import lazy_let\n");
        let diags = check_unknown_modules(
            "t.ty",
            "from typhon_runtime.lazy import lazy_let\n",
            &module,
            &[],
            &[],
        );
        assert!(diags.warnings().is_empty(), "{:?}", diags.warnings());
    }

    #[test]
    fn declared_dependency_is_clean() {
        let module = parse_module("from pandas import DataFrame\n");
        let diags = check_unknown_modules(
            "t.ty",
            "from pandas import DataFrame\n",
            &module,
            &[],
            &["pandas".to_string()],
        );
        assert!(diags.warnings().is_empty(), "{:?}", diags.warnings());
    }

    #[test]
    fn relative_import_does_not_warn() {
        // Relative imports (`from .sibling import X`) aren't vettable
        // here — we don't model relative resolution. Just trust them.
        let module = parse_module("from . import sibling\n");
        let diags = check_unknown_modules("t.ty", "from . import sibling\n", &module, &[], &[]);
        assert!(diags.warnings().is_empty(), "{:?}", diags.warnings());
    }

    #[test]
    fn imports_in_nested_blocks_are_walked() {
        // gemini-code-assist review on PR #68 (tyc-resolve L382): the
        // initial walk skipped `for` / `while` / `with` / `match` /
        // `try`-handler bodies. Verify each shape surfaces an unknown
        // module from a nested import.
        let cases: &[(&str, &str)] = &[
            ("in `for`", "for x in []:\n    from notamod_for import a\n"),
            (
                "in `while`",
                "while False:\n    from notamod_while import b\n",
            ),
            (
                "in `with`",
                "with open('x') as f:\n    from notamod_with import c\n",
            ),
            (
                "in `match`",
                "match 1:\n    case _:\n        from notamod_match import d\n",
            ),
            (
                "in `try` handler",
                "try:\n    pass\nexcept Exception:\n    from notamod_except import e\n",
            ),
        ];
        for (label, src) in cases {
            let module = parse_module(src);
            let diags = check_unknown_modules("t.ty", src, &module, &[], &[]);
            assert!(
                diags
                    .warnings()
                    .iter()
                    .any(|e| matches!(e, TycError::UnknownModule { .. })),
                "expected UnknownModule {label}; got {:?}",
                diags.warnings()
            );
        }
    }

    #[test]
    fn typevar_import_is_rejected() {
        // FINDINGS #73: `from typing import TypeVar` must surface a
        // dedicated diagnostic that points users at PEP 695 syntax.
        let (_m, d) = resolve("from typing import TypeVar\n");
        assert!(
            d.errors()
                .iter()
                .any(|e| matches!(e, TycError::TypeVarImportRejected { .. })),
            "expected TypeVarImportRejected variant; got {:?}",
            d.errors()
        );
    }

    #[test]
    fn typing_list_alias_is_warned() {
        // FINDINGS #74: prefer lowercase `list` over `typing.List`. The
        // warning anchors on the imported name; the import itself still
        // succeeds so existing code keeps compiling — projects that
        // promote warnings to errors will catch it in CI.
        let (_m, d) = resolve("from typing import List\nlet xs: List[int] = []\n");
        assert!(
            d.warnings()
                .iter()
                .any(|e| matches!(e, TycError::TypingAliasDeprecated { .. })),
            "expected TypingAliasDeprecated warning; got warnings={:?} errors={:?}",
            d.warnings(),
            d.errors()
        );
    }

    #[test]
    fn typing_optional_is_not_flagged() {
        // `Optional` / `Union` / `Callable` are not lowercase-aliased —
        // they have their own Typhon-native shapes (`T?`, `T | U`,
        // `Callable[[A], B]`), but the import itself is not deprecated.
        let (_m, d) = resolve("from typing import Optional, Callable\n");
        assert!(
            !d.warnings()
                .iter()
                .any(|e| matches!(e, TycError::TypingAliasDeprecated { .. })),
            "Optional/Callable must not trigger the alias warning; got {:?}",
            d.warnings()
        );
    }

    #[test]
    fn for_loop_target_reassignment_is_rejected() {
        // FINDINGS #75: the for-loop target is bindable as immutable
        // (Rule 2) — `i = i + 1` inside the body must error rather
        // than silently shadowing the loop variable.
        let src = "def main() -> None:\n\
                   \x20   let xs: list[int] = [1, 2, 3]\n\
                   \x20   for i in xs:\n\
                   \x20       i = i + 1\n";
        let (_m, d) = resolve(src);
        assert!(
            d.errors()
                .iter()
                .any(|e| matches!(e, TycError::ImmutableAssign { name, .. } if name == "i")),
            "expected ImmutableAssign on `i` rebind; got {:?}",
            d.errors()
        );
    }

    #[test]
    fn for_loop_iteration_itself_is_clean() {
        // Sanity: declaring the target should not itself trip
        // immutable_assign — only user reassignments inside the body do.
        let src = "def main() -> None:\n\
                   \x20   let xs: list[int] = [1, 2, 3]\n\
                   \x20   for i in xs:\n\
                   \x20       print(i)\n";
        let (_m, d) = resolve(src);
        assert!(
            !d.has_errors(),
            "loop without rebind should be clean: {:?}",
            d.errors()
        );
    }

    #[test]
    fn let_without_initialiser_errors() {
        // FINDINGS #91: `let x: int` (no `=`) must surface
        // tyc::missing_initialiser rather than silently accepting the
        // declaration and then complaining about the user's first
        // `x = …` assignment as an immutable-assign error.
        let (_m, d) = resolve("def f() -> None:\n    let x: int\n    x = 5\n");
        assert!(
            d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingInitialiser { .. })),
            "expected MissingInitialiser variant; got {:?}",
            d.errors()
        );
    }

    #[test]
    fn mut_without_initialiser_errors() {
        // Same rule applies to `mut x: int` — the binding has nothing
        // to bind to. Subsequent `x = 5` would otherwise pass because
        // `mut` allows re-assignment, but the type-checker treats the
        // value-less form inconsistently.
        let (_m, d) = resolve("def f() -> None:\n    mut x: int\n    x = 5\n");
        assert!(
            d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingInitialiser { .. })),
            "expected MissingInitialiser for mut; got {:?}",
            d.errors()
        );
    }

    #[test]
    fn class_field_without_initialiser_is_clean() {
        // Dataclass-style field declarations inside a `class` body
        // legitimately omit initialisers — the constructor produces
        // the value, no `= <expr>` is required.
        let (_m, d) = resolve("class Point:\n    x: int\n    y: int\n");
        assert!(
            !d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingInitialiser { .. })),
            "class field declaration must not fire missing_initialiser: {:?}",
            d.errors()
        );
    }

    #[test]
    fn self_outside_impl_uses_dedicated_diagnostic() {
        // FINDINGS #90: `self` outside an `impl` method body must surface
        // `tyc::self_outside_impl`, not the generic
        // `tyc::unknown_name` whose help text would push the user
        // toward `let self = …` (which doesn't fix the problem).
        let (_m, d) = resolve("def f() -> int:\n    return self.x\n");
        assert!(d.has_errors(), "self outside impl must be an error");
        assert!(
            d.errors()
                .iter()
                .any(|e| matches!(e, TycError::SelfOutsideImpl { .. })),
            "expected SelfOutsideImpl variant; got {:?}",
            d.errors()
        );
    }

    #[test]
    fn function_local_bareword_assign_requires_binding_kind() {
        // Rule 2: locals must carry `let` or `mut`. Module scope still
        // defaults to `let`, so this only fires at function scope.
        let (_m, d) = resolve("def f() -> None:\n    counter = 1\n");
        assert!(d.has_errors(), "bareword local must error");
        assert!(
            d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingBindingKind { .. })),
            "expected MissingBindingKind variant, got {:?}",
            d.errors()
        );
    }

    #[test]
    fn function_local_let_assignment_is_clean() {
        // Sanity: explicit `let` does not trigger missing_binding_kind.
        let (_m, d) = resolve("def f() -> None:\n    let counter = 1\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn function_local_mut_assignment_is_clean() {
        let (_m, d) = resolve("def f() -> None:\n    mut counter = 0\n    counter = counter + 1\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn module_level_bareword_assign_still_clean() {
        // Rule 2 only applies at function scope; module-level bindings
        // default to `let` and are exempt.
        let (_m, d) = resolve("counter = 1\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn synthetic_temp_assignment_is_exempt() {
        // Compiler-synthesised `__typhon_*` temps (e.g. the `?` operator's
        // `__typhon_q_N__`) must not trigger the diagnostic — the
        // user-source spelling is `expr?`, never a bare assignment.
        let (_m, d) =
            resolve("def f() -> None:\n    __typhon_q_0__ = 1\n    print(__typhon_q_0__)\n");
        assert!(
            !d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingBindingKind { .. })),
            "synthesised temp must not fire MissingBindingKind: {:?}",
            d.errors()
        );
    }

    #[test]
    fn function_local_rebind_inherits_kind_without_diagnostic() {
        // The first declaration carries the keyword; later bareword
        // assignments to the same name in the same scope inherit the
        // existing binding's mutability and don't re-trigger the
        // diagnostic. (Reassignment of a `let` still produces
        // `immutable_assign`, which is the right diagnostic for that case.)
        let (_m, d) = resolve("def f() -> None:\n    mut counter = 0\n    counter = counter + 1\n");
        assert!(
            !d.errors()
                .iter()
                .any(|e| matches!(e, TycError::MissingBindingKind { .. })),
            "rebind of existing local must not fire MissingBindingKind: {:?}",
            d.errors()
        );
    }

    #[test]
    fn unknown_name_inside_fstring_interpolation_is_flagged() {
        // ruff's FString embeds the interpolated expression inside
        // `value.elements()` rather than exposing it as a top-level
        // `Expr`, so the resolver must explicitly walk the InterpolatedStringElement
        // tree. Otherwise unknown names inside `f"{…}"` go undetected.
        let (_m, d) = resolve("x = f\"{missing_name}\"\n");
        assert!(d.has_errors(), "f-string interpolation must be walked");
        let msg = format!("{}", d.errors()[0]);
        assert!(
            msg.contains("cannot find 'missing_name'"),
            "expected the unknown-name diagnostic to fire on the interpolation; got {}",
            msg
        );
    }

    #[test]
    fn builtin_print_is_in_scope() {
        let (_m, d) = resolve("def f() -> None:\n    print(1)\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn self_in_impl_method_body_not_flagged() {
        // Simulates what the preprocessor produces from `impl User: def greet():`.
        // `self` is injected by the desugar pass; the resolver must not flag it
        // as unknown when it appears inside an impl pseudo-class method body.
        let (_m, d) = resolve(
            "class __typhon_impl_User(object):\n    def greet():\n        return self.name\n",
        );
        assert!(
            !d.has_errors(),
            "self inside impl method must not be unknown: {:?}",
            d.errors()
        );
    }

    #[test]
    fn self_outside_impl_method_is_unknown() {
        // `self` used in a plain module-level function must still be flagged.
        let (_m, d) = resolve("def f():\n    return self\n");
        assert!(d.has_errors(), "self outside impl must be an unknown name");
        assert!(
            d.errors().iter().any(|e| format!("{e}").contains("'self'")),
            "error must mention 'self'; errors: {:?}",
            d.errors()
        );
    }

    #[test]
    fn parameters_resolved() {
        let (_m, d) = resolve("def f(x: int) -> int:\n    return x\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn function_introduces_scope() {
        let (m, _d) = resolve("def f() -> None:\n    let x: int = 1\n    print(x)\n");
        // Module scope has `f`; inner scope has `x`.
        assert!(m.module_scope().lookup_local("f").is_some());
        let fn_scope = m
            .scopes
            .iter()
            .find(|s| s.kind == ScopeKind::Function)
            .unwrap();
        assert!(fn_scope.lookup_local("x").is_some());
    }

    #[test]
    fn dotted_import_binds_top_level_package() {
        let (m, d) = resolve("import os.path\nlet n: int = len(os.path.sep)\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
        // Python binds `os`, not `os.path`.
        assert!(m.module_scope().lookup_local("os").is_some());
        assert!(m.module_scope().lookup_local("os.path").is_none());
    }

    #[test]
    fn def_collision_with_let_errors() {
        let (_m, d) = resolve("let x: int = 1\ndef x() -> None:\n    pass\n");
        assert!(d.has_errors(), "expected val/def collision");
    }

    #[test]
    fn for_loop_target_cannot_rebind_let() {
        let src = "let items: list = []\nfor items in [[1]]:\n    pass\n";
        let (_m, d) = resolve(src);
        assert!(d.has_errors(), "expected for-loop rebinding to error");
    }

    #[test]
    fn parameter_annotation_references_resolved() {
        // A missing annotation type should now surface as an unknown name.
        let (_m, d) = resolve("def f(x: NoSuchType) -> None:\n    pass\n");
        assert!(
            d.has_errors(),
            "expected unknown type in parameter annotation"
        );
        let msg = format!("{}", d.errors()[0]);
        assert!(msg.contains("NoSuchType"), "got {}", msg);
    }

    // ── unused import detection ──────────────────────────────────────────────

    #[test]
    fn unused_import_is_a_warning() {
        let (_m, d) = resolve("import os\n");
        assert!(!d.has_errors(), "unused import should not be an error");
        assert_eq!(d.warning_count(), 1, "expected exactly one warning");
        let msg = format!("{}", d.warnings()[0]);
        assert!(
            msg.contains("os"),
            "warning should name the import, got: {msg}"
        );
    }

    #[test]
    fn used_import_has_no_warning() {
        let (_m, d) = resolve("import os\nlet n: int = len(os.sep)\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
        assert_eq!(d.warning_count(), 0, "used import must not warn");
    }

    #[test]
    fn unused_from_import_warns() {
        let (_m, d) = resolve("from os import path\n");
        assert_eq!(d.warning_count(), 1);
        let msg = format!("{}", d.warnings()[0]);
        assert!(msg.contains("path"), "got: {msg}");
    }

    #[test]
    fn used_from_import_no_warning() {
        let (_m, d) = resolve("from os import path\nlet s: str = path.sep\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
        assert_eq!(d.warning_count(), 0);
    }

    #[test]
    fn import_as_alias_unused_warns() {
        let (_m, d) = resolve("import os.path as osp\n");
        assert_eq!(d.warning_count(), 1);
        let msg = format!("{}", d.warnings()[0]);
        assert!(msg.contains("osp"), "got: {msg}");
    }

    #[test]
    fn import_as_alias_used_no_warning() {
        let (_m, d) = resolve("import os.path as osp\nlet s: str = osp.sep\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
        assert_eq!(d.warning_count(), 0);
    }

    #[test]
    fn multiple_imports_only_unused_warns() {
        let src = "import os\nimport sys\nlet p: str = sys.version\n";
        let (_m, d) = resolve(src);
        assert_eq!(d.warning_count(), 1, "only `os` should warn");
        let msg = format!("{}", d.warnings()[0]);
        assert!(msg.contains("os"), "got: {msg}");
    }

    #[test]
    fn import_shadowed_by_parameter_still_warns() {
        // The `os` reference inside `f` resolves to the parameter, not the
        // import.  The import at module scope is never the resolved target of
        // any reference, so it must still warn as unused.
        let src = "import os\ndef f(os: str) -> None:\n    print(os)\n";
        let (_m, d) = resolve(src);
        assert_eq!(d.warning_count(), 1, "shadowed import must still warn");
        let msg = format!("{}", d.warnings()[0]);
        assert!(msg.contains("os"), "got: {msg}");
    }

    #[test]
    fn underscore_prefixed_import_not_warned() {
        // `_unused` is the conventional marker for intentionally-unused names.
        let src = "import os as _unused\nlet x: int = 1\n";
        let (_m, d) = resolve(src);
        assert_eq!(d.warning_count(), 0, "_-prefixed import must not warn");
    }

    #[test]
    fn symbol_at_offset_finds_reference() {
        // `val x: int = 1\nlet y: int = x\n`
        //   index in preprocessed source:
        //   "x: int = 1\ny: int = x\n"
        //       column 0..1 is `x` (declaration), column 11 is `y` (declaration),
        //       column 20 is the reference `x` on the second line.
        let src = "let x: int = 1\nlet y: int = x\n";
        let (m, _d) = resolve(src);
        // The byte offset of the reference `x` on the second line: the
        // source is "let x: int = 1\nlet y: int = x\n" (preprocessor no
        // longer strips let/mut). First line ends at byte 14 (newline
        // inclusive at 14), second line `let y: int = x` puts `x` at
        // byte 15 + 13 = 28.
        let symbol = m
            .symbol_at_offset(28)
            .expect("symbol_at_offset should find the reference");
        assert_eq!(symbol.name, "x");
        assert!(!symbol.is_definition, "this is a use site, not a decl");
        let def = symbol.definition.expect("reference should resolve");
        assert_eq!(def.name, "x");
        assert_eq!(def.mutability, Mutability::Let);
    }

    #[test]
    fn symbol_at_offset_finds_declaration() {
        let src = "let foo: int = 1\n";
        let (m, _d) = resolve(src);
        // In the (unstripped) source `let foo: int = 1\n`, `foo` starts at byte 4.
        let symbol = m.symbol_at_offset(5).expect("symbol should be found");
        assert_eq!(symbol.name, "foo");
        assert!(
            symbol.is_definition,
            "offset inside a binding span should be marked as definition"
        );
    }

    #[test]
    fn symbol_at_offset_returns_none_far_past_source() {
        let src = "let x: int = 1\n";
        let (m, _d) = resolve(src);
        // An offset well past the end of every binding range must not match.
        assert!(
            m.symbol_at_offset(10_000).is_none(),
            "offsets past source end should not resolve to any symbol"
        );
    }
}
