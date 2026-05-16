//! Name resolution and scope construction for Typhon.
//!
//! Walks a parsed Python module and produces:
//!
//! - A tree of [`Scope`]s rooted at the module scope.
//! - A [`SymbolTable`] that maps every introduced name to its declaration.
//! - A set of [`Reference`]s recording each use of a name.
//! - Diagnostics for unknown names and `val` re-assignments.
//!
//! The resolver consumes the original Typhon source plus the parsed Python
//! AST. The Python AST has byte offsets relative to the *preprocessed*
//! source, but the val/var stripping never alters line numbers and only
//! removes characters at the start of a line, so positions inside
//! expressions remain stable; we use them directly.

use rustpython_ast::text_size::TextRange;
use rustpython_ast::{Expr, Mod, Stmt};
use tyc_diagnostics::{Diagnostics, TycError};
use tyc_syntax::lexer::TyphonKeyword;
use tyc_syntax::preprocess::StrippedKeyword;

/// Mutability of a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutability {
    /// `val` — immutable; reassignment is a compile error.
    Val,
    /// `var`, function/class declaration, parameter, or import — mutable
    /// or rebindable by the language semantics. Only `val` is rejected on
    /// reassignment.
    Var,
}

/// What kind of entity a binding introduces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingKind {
    /// A `val` or `var` value binding (annotated or not).
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

/// One name introduced in some scope.
#[derive(Debug, Clone)]
pub struct Binding {
    pub name: String,
    pub kind: BindingKind,
    pub mutability: Mutability,
    /// Byte range of the declaration site in the preprocessed source.
    pub span: (usize, usize),
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
}

impl Scope {
    pub fn lookup_local(&self, name: &str) -> Option<&Binding> {
        self.bindings.iter().find(|b| b.name == name)
    }
}

/// The resolved structure of a module: scopes, bindings, references, plus
/// the list of `(declaration_offset, mutability)` pairs for every binding
/// so the type checker can find them again later.
#[derive(Debug, Clone, Default)]
pub struct ResolvedModule {
    pub scopes: Vec<Scope>,
    pub references: Vec<Reference>,
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
}

/// Internal helper for building a [`ResolvedModule`] while walking the AST.
struct Resolver<'a> {
    path: String,
    source: &'a str,
    /// `stripped[i]` records that line `i` had a leading `val`/`var`. The
    /// list is in source order; we consume entries as we hit assignments.
    stripped: &'a [StrippedKeyword],
    /// Byte offset of the first character on each line, computed once so
    /// that `line_of_offset` is O(log N) per call instead of O(N).
    line_starts: Vec<usize>,
    scopes: Vec<Scope>,
    references: Vec<Reference>,
    diagnostics: Diagnostics,
}

impl<'a> Resolver<'a> {
    fn new(path: String, source: &'a str, stripped: &'a [StrippedKeyword]) -> Self {
        let module = Scope {
            id: 0,
            kind: ScopeKind::Module,
            parent: None,
            bindings: Vec::new(),
        };
        let mut line_starts = vec![0usize];
        for (idx, b) in source.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(idx + 1);
            }
        }
        Self {
            path,
            source,
            stripped,
            line_starts,
            scopes: vec![module],
            references: Vec::new(),
            diagnostics: Diagnostics::new(),
        }
    }

    fn push_scope(&mut self, kind: ScopeKind, parent: ScopeId) -> ScopeId {
        let id = self.scopes.len();
        self.scopes.push(Scope {
            id,
            kind,
            parent: Some(parent),
            bindings: Vec::new(),
        });
        id
    }

    /// Map a byte offset in the preprocessed source to a 0-based line index.
    /// O(log N) via binary search over precomputed line starts.
    fn line_of_offset(&self, offset: usize) -> usize {
        self.line_starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1)
    }

    /// Was the assignment statement on `line_idx` introduced with `val` or
    /// `var`?
    fn keyword_for_line(&self, line_idx: usize) -> Option<TyphonKeyword> {
        self.stripped
            .iter()
            .find(|sk| sk.line_index == line_idx)
            .map(|sk| sk.keyword)
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
            if existing.mutability == Mutability::Val || mutability == Mutability::Val {
                let decl_span = existing.span;
                self.diagnostics.push_error(TycError::immutable_assign(
                    name,
                    &self.path,
                    self.source,
                    decl_span.0,
                    decl_span.1.saturating_sub(decl_span.0).max(1),
                    span.0,
                    span.1.saturating_sub(span.0).max(1),
                ));
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

/// Resolve a parsed module and return scopes + diagnostics.
pub fn resolve_module(
    path: impl Into<String>,
    source: &str,
    stripped: &[StrippedKeyword],
    module: &Mod,
) -> (ResolvedModule, Diagnostics) {
    let mut r = Resolver::new(path.into(), source, stripped);

    if let Mod::Module(m) = module {
        // First pass: collect top-level declarations so forward references
        // inside functions and classes resolve correctly.
        collect_top_level(&mut r, 0, &m.body);

        // Second pass: walk bodies to record references and inner scopes.
        for stmt in &m.body {
            walk_stmt(&mut r, 0, stmt);
        }
    }

    r.report_unknown_names();

    let resolved = ResolvedModule {
        scopes: std::mem::take(&mut r.scopes),
        references: std::mem::take(&mut r.references),
    };
    (resolved, r.diagnostics)
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
/// later `def x` or `class x` collides with an earlier `val x`.
fn collect_top_level(r: &mut Resolver, scope: ScopeId, body: &[Stmt<TextRange>]) {
    // Sub-pass 1: value bindings (so val-protection sees them first).
    let default_val = r.scopes[scope].kind == ScopeKind::Module;
    for stmt in body {
        match stmt {
            Stmt::Assign(a) => {
                for t in &a.targets {
                    declare_target(r, scope, t, default_val);
                }
            }
            Stmt::AnnAssign(a) => {
                declare_target(r, scope, &a.target, default_val);
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
                r.declare(scope, f.name.as_str(), BindingKind::Function, Mutability::Var, span);
            }
            Stmt::AsyncFunctionDef(f) => {
                let span = find_def_name_span(
                    r.source,
                    f.range.start().to_usize(),
                    "def ",
                    f.name.as_str(),
                );
                r.declare(scope, f.name.as_str(), BindingKind::Function, Mutability::Var, span);
            }
            Stmt::ClassDef(c) => {
                let span = find_def_name_span(
                    r.source,
                    c.range.start().to_usize(),
                    "class ",
                    c.name.as_str(),
                );
                r.declare(scope, c.name.as_str(), BindingKind::Class, Mutability::Var, span);
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
                    r.declare(scope, &bound_name, BindingKind::Import, Mutability::Var, span);
                }
            }
            Stmt::ImportFrom(i) => {
                for alias in &i.names {
                    let name = alias.asname.as_ref().unwrap_or(&alias.name);
                    let span = (
                        alias.range.start().to_usize(),
                        alias.range.start().to_usize() + name.as_str().len(),
                    );
                    r.declare(scope, name.as_str(), BindingKind::Import, Mutability::Var, span);
                }
            }
            _ => {}
        }
    }
}

fn declare_target(
    r: &mut Resolver,
    scope: ScopeId,
    target: &Expr<TextRange>,
    default_val: bool,
) {
    if let Expr::Name(n) = target {
        let line = r.line_of_offset(n.range.start().to_usize());
        let kw = r.keyword_for_line(line);
        // When no explicit `val`/`var` keyword is present, treat a bare
        // assignment as a rebinding of any existing binding (taking its
        // mutability) rather than a fresh declaration. Only the *first*
        // bareword assignment in a module scope defaults to `val`; later
        // bare assignments inherit the existing binding's mutability.
        let existing_mut = r.lookup_local(scope, n.id.as_str()).map(|b| b.mutability);
        let mutability = match kw {
            Some(TyphonKeyword::Val) => Mutability::Val,
            Some(TyphonKeyword::Var) => Mutability::Var,
            None => existing_mut.unwrap_or(if default_val {
                Mutability::Val
            } else {
                Mutability::Var
            }),
        };
        let span = (
            n.range.start().to_usize(),
            n.range.start().to_usize() + n.id.as_str().len(),
        );
        r.declare(scope, n.id.as_str(), BindingKind::Value, mutability, span);
    }
}

/// Walk a statement, recording references to names and descending into
/// nested function/class scopes.
fn walk_stmt(r: &mut Resolver, scope: ScopeId, stmt: &Stmt<TextRange>) {
    match stmt {
        Stmt::FunctionDef(f) => {
            // Decorators are evaluated in the enclosing scope.
            for d in &f.decorator_list {
                walk_expr(r, scope, d);
            }
            // Parameter and return annotations live in the enclosing scope
            // (PEP 563 style); record references to every type they mention.
            walk_argument_annotations(r, scope, &f.args);
            if let Some(ret) = &f.returns {
                walk_expr(r, scope, ret);
            }
            let fn_scope = r.push_scope(ScopeKind::Function, scope);
            // Parameters become bindings in the new scope.
            declare_arguments(r, fn_scope, &f.args);
            // Pre-collect declarations within the function body so forward
            // references work.
            collect_top_level(r, fn_scope, &f.body);
            for s in &f.body {
                walk_stmt(r, fn_scope, s);
            }
        }
        Stmt::AsyncFunctionDef(f) => {
            for d in &f.decorator_list {
                walk_expr(r, scope, d);
            }
            walk_argument_annotations(r, scope, &f.args);
            if let Some(ret) = &f.returns {
                walk_expr(r, scope, ret);
            }
            let fn_scope = r.push_scope(ScopeKind::Function, scope);
            declare_arguments(r, fn_scope, &f.args);
            collect_top_level(r, fn_scope, &f.body);
            for s in &f.body {
                walk_stmt(r, fn_scope, s);
            }
        }
        Stmt::ClassDef(c) => {
            for d in &c.decorator_list {
                walk_expr(r, scope, d);
            }
            for base in &c.bases {
                walk_expr(r, scope, base);
            }
            let cls_scope = r.push_scope(ScopeKind::Class, scope);
            collect_top_level(r, cls_scope, &c.body);
            for s in &c.body {
                walk_stmt(r, cls_scope, s);
            }
        }
        Stmt::Assign(a) => {
            walk_expr(r, scope, &a.value);
            let default_val = r.scopes[scope].kind == ScopeKind::Module;
            for t in &a.targets {
                if let Expr::Name(_) = t {
                    declare_target(r, scope, t, default_val);
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
            if let Expr::Name(_) = a.target.as_ref() {
                let default_val = r.scopes[scope].kind == ScopeKind::Module;
                declare_target(r, scope, &a.target, default_val);
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
            for s in &i.orelse {
                walk_stmt(r, scope, s);
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
            // Loop target introduces a binding.
            if let Expr::Name(n) = f.target.as_ref() {
                let span = (
                    n.range.start().to_usize(),
                    n.range.start().to_usize() + n.id.as_str().len(),
                );
                r.declare(scope, n.id.as_str(), BindingKind::Loop, Mutability::Var, span);
            }
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
                    if let Expr::Name(n) = var.as_ref() {
                        let span = (
                            n.range.start().to_usize(),
                            n.range.start().to_usize() + n.id.as_str().len(),
                        );
                        r.declare(scope, n.id.as_str(), BindingKind::Loop, Mutability::Var, span);
                    }
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
                let rustpython_ast::ExceptHandler::ExceptHandler(h) = h;
                if let Some(typ) = &h.type_ {
                    walk_expr(r, scope, typ);
                }
                if let Some(name) = &h.name {
                    let span = (
                        h.range.start().to_usize(),
                        h.range.start().to_usize() + name.as_str().len(),
                    );
                    r.declare(scope, name.as_str(), BindingKind::Loop, Mutability::Var, span);
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
        Stmt::Global(_) | Stmt::Nonlocal(_) => {}
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
        _ => {}
    }
}

/// Walk every annotation expression on the parameters of a function, so
/// names used in those annotations are recorded as references and bound
/// against the enclosing scope.
fn walk_argument_annotations(
    r: &mut Resolver,
    scope: ScopeId,
    args: &rustpython_ast::Arguments<TextRange>,
) {
    let all = args
        .posonlyargs
        .iter()
        .chain(args.args.iter())
        .chain(args.kwonlyargs.iter());
    for arg in all {
        if let Some(ann) = &arg.def.annotation {
            walk_expr(r, scope, ann);
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

fn declare_arguments(
    r: &mut Resolver,
    scope: ScopeId,
    args: &rustpython_ast::Arguments<TextRange>,
) {
    let all = args
        .posonlyargs
        .iter()
        .chain(args.args.iter())
        .chain(args.kwonlyargs.iter());
    for arg in all {
        let span = (
            arg.def.range.start().to_usize(),
            arg.def.range.start().to_usize() + arg.def.arg.as_str().len(),
        );
        r.declare(
            scope,
            arg.def.arg.as_str(),
            BindingKind::Parameter,
            Mutability::Var,
            span,
        );
    }
    if let Some(va) = &args.vararg {
        let span = (
            va.range.start().to_usize(),
            va.range.start().to_usize() + va.arg.as_str().len(),
        );
        r.declare(scope, va.arg.as_str(), BindingKind::Parameter, Mutability::Var, span);
    }
    if let Some(kw) = &args.kwarg {
        let span = (
            kw.range.start().to_usize(),
            kw.range.start().to_usize() + kw.arg.as_str().len(),
        );
        r.declare(scope, kw.arg.as_str(), BindingKind::Parameter, Mutability::Var, span);
    }
}

/// Walk an expression, recording every name reference.
fn walk_expr(r: &mut Resolver, scope: ScopeId, expr: &Expr<TextRange>) {
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
            for a in &c.args {
                walk_expr(r, scope, a);
            }
            for k in &c.keywords {
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
            for c2 in &c.comparators {
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
            for k in d.keys.iter().flatten() {
                walk_expr(r, scope, k);
            }
            for v in &d.values {
                walk_expr(r, scope, v);
            }
        }
        Expr::IfExp(i) => {
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
            let scope2 = r.push_scope(ScopeKind::Function, scope);
            declare_arguments(r, scope2, &l.args);
            walk_expr(r, scope2, &l.body);
        }
        Expr::ListComp(c) => walk_comp(r, scope, &c.elt, &c.generators),
        Expr::SetComp(c) => walk_comp(r, scope, &c.elt, &c.generators),
        Expr::GeneratorExp(g) => walk_comp(r, scope, &g.elt, &g.generators),
        Expr::DictComp(c) => {
            let scope2 = r.push_scope(ScopeKind::Comprehension, scope);
            for gen in &c.generators {
                walk_expr(r, scope2, &gen.iter);
                if let Expr::Name(n) = &gen.target {
                    let span = (
                        n.range.start().to_usize(),
                        n.range.start().to_usize() + n.id.as_str().len(),
                    );
                    r.declare(scope2, n.id.as_str(), BindingKind::Loop, Mutability::Var, span);
                }
                for cond in &gen.ifs {
                    walk_expr(r, scope2, cond);
                }
            }
            walk_expr(r, scope2, &c.key);
            walk_expr(r, scope2, &c.value);
        }
        Expr::Constant(_) | Expr::JoinedStr(_) | Expr::FormattedValue(_) => {}
        Expr::NamedExpr(n) => {
            walk_expr(r, scope, &n.value);
            if let Expr::Name(name) = n.target.as_ref() {
                let span = (
                    name.range.start().to_usize(),
                    name.range.start().to_usize() + name.id.as_str().len(),
                );
                r.declare(scope, name.id.as_str(), BindingKind::Value, Mutability::Var, span);
            }
        }
    }
}

fn walk_comp(
    r: &mut Resolver,
    scope: ScopeId,
    elt: &Expr<TextRange>,
    generators: &[rustpython_ast::Comprehension<TextRange>],
) {
    let scope2 = r.push_scope(ScopeKind::Comprehension, scope);
    for gen in generators {
        walk_expr(r, scope2, &gen.iter);
        if let Expr::Name(n) = &gen.target {
            let span = (
                n.range.start().to_usize(),
                n.range.start().to_usize() + n.id.as_str().len(),
            );
            r.declare(scope2, n.id.as_str(), BindingKind::Loop, Mutability::Var, span);
        }
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
        "print", "len", "range", "abs", "min", "max", "sum", "any", "all",
        "sorted", "reversed", "enumerate", "zip", "map", "filter", "isinstance",
        "issubclass", "hasattr", "getattr", "setattr", "delattr", "iter", "next",
        "repr", "id", "hash", "type", "vars", "dir", "callable", "input",
        "open", "exit", "quit", "breakpoint", "format", "ord", "chr", "hex",
        "oct", "bin", "round", "pow", "divmod", "globals", "locals", "eval",
        "exec", "compile", "object", "super", "property", "classmethod",
        "staticmethod", "frozenset",
        // Built-in types
        "int", "str", "bool", "float", "complex", "bytes", "bytearray", "memoryview",
        "list", "tuple", "set", "dict", "type",
        // Constants
        "True", "False", "None", "Ellipsis", "NotImplemented",
        "__name__", "__file__", "__doc__", "__builtins__", "__package__",
        "__loader__", "__spec__", "__debug__",
        // Common exceptions
        "Exception", "BaseException", "ValueError", "TypeError", "KeyError",
        "IndexError", "AttributeError", "RuntimeError", "StopIteration",
        "StopAsyncIteration", "GeneratorExit", "FileNotFoundError",
        "FileExistsError", "PermissionError", "NotImplementedError",
        "ZeroDivisionError", "OverflowError", "ArithmeticError",
        "OSError", "IOError", "ImportError", "ModuleNotFoundError",
        "LookupError", "NameError", "UnicodeError", "UnicodeDecodeError",
        "UnicodeEncodeError", "AssertionError", "SyntaxError",
        "IndentationError", "TabError", "SystemError", "SystemExit",
        "KeyboardInterrupt", "MemoryError", "RecursionError",
        // Phase-1 typing names commonly used in annotations
        "Optional", "Union", "Any", "Callable", "Iterable", "Iterator",
        "Sequence", "Mapping", "MutableMapping", "List", "Dict", "Set",
        "Tuple", "FrozenSet", "Type", "TypeVar", "Generic", "Protocol",
        "Self", "ClassVar", "Final", "Literal", "NoReturn", "Awaitable",
        "Coroutine", "Generator", "AsyncIterator", "AsyncIterable",
        // Typhon Result type constructors (from typhon_runtime).
        "Ok", "Err", "Result",
    ];
    names.iter().copied().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpython_parser::{parse, Mode};
    use tyc_syntax::preprocess::preprocess;

    fn resolve(src: &str) -> (ResolvedModule, Diagnostics) {
        let prep = preprocess(src);
        let module = parse(&prep.python_source, Mode::Module, "<test>").unwrap();
        resolve_module("<test>".to_owned(), &prep.python_source, &prep.stripped, &module)
    }

    #[test]
    fn collects_val_binding() {
        let (m, d) = resolve("val x: int = 1\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
        let scope = m.module_scope();
        let x = scope.lookup_local("x").unwrap();
        assert_eq!(x.mutability, Mutability::Val);
    }

    #[test]
    fn collects_var_binding() {
        let (m, d) = resolve("var count: int = 0\n");
        assert!(!d.has_errors());
        let count = m.module_scope().lookup_local("count").unwrap();
        assert_eq!(count.mutability, Mutability::Var);
    }

    #[test]
    fn val_reassignment_is_an_error() {
        let (_m, d) = resolve("val x: int = 1\nx = 2\n");
        assert!(d.has_errors());
        let msg = format!("{}", d.errors()[0]);
        assert!(msg.contains("cannot assign to immutable binding 'x'"), "got {}", msg);
    }

    #[test]
    fn var_reassignment_is_ok() {
        let (_m, d) = resolve("var x: int = 1\nx = 2\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn unknown_name_errors() {
        let (_m, d) = resolve("y = z + 1\n");
        assert!(d.has_errors());
        let msg = format!("{}", d.errors()[0]);
        assert!(msg.contains("cannot find 'z'"), "got {}", msg);
    }

    #[test]
    fn builtin_print_is_in_scope() {
        let (_m, d) = resolve("def f() -> None:\n    print(1)\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn parameters_resolved() {
        let (_m, d) = resolve("def f(x: int) -> int:\n    return x\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
    }

    #[test]
    fn function_introduces_scope() {
        let (m, _d) = resolve("def f() -> None:\n    val x: int = 1\n    print(x)\n");
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
        let (m, d) = resolve("import os.path\nval n: int = len(os.path.sep)\n");
        assert!(!d.has_errors(), "{:?}", d.errors());
        // Python binds `os`, not `os.path`.
        assert!(m.module_scope().lookup_local("os").is_some());
        assert!(m.module_scope().lookup_local("os.path").is_none());
    }

    #[test]
    fn def_collision_with_val_errors() {
        let (_m, d) = resolve("val x: int = 1\ndef x() -> None:\n    pass\n");
        assert!(d.has_errors(), "expected val/def collision");
    }

    #[test]
    fn for_loop_target_cannot_rebind_val() {
        let src = "val items: list = []\nfor items in [[1]]:\n    pass\n";
        let (_m, d) = resolve(src);
        assert!(d.has_errors(), "expected for-loop rebinding to error");
    }

    #[test]
    fn parameter_annotation_references_resolved() {
        // A missing annotation type should now surface as an unknown name.
        let (_m, d) = resolve("def f(x: NoSuchType) -> None:\n    pass\n");
        assert!(d.has_errors(), "expected unknown type in parameter annotation");
        let msg = format!("{}", d.errors()[0]);
        assert!(msg.contains("NoSuchType"), "got {}", msg);
    }
}
