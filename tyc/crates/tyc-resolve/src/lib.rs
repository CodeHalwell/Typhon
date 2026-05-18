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
use ruff_text_size::TextRange;
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
}

impl<'a> Resolver<'a> {
    fn new(path: String, source: &'a str) -> Self {
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
        }
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
        self.declare_with(scope, name, kind, mutability, span, None);
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
            import_info,
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
}

/// Resolve a parsed module and return scopes + diagnostics.
pub fn resolve_module(
    path: String,
    source: &str,
    module: &ModModule,
) -> (ResolvedModule, Diagnostics) {
    let mut r = Resolver::new(path, source);

    // First pass: collect top-level declarations so forward references
    // inside functions and classes resolve correctly.
    collect_top_level(&mut r, 0, &module.body);

    // Second pass: walk bodies to record references and inner scopes.
    for stmt in &module.body {
        walk_stmt(&mut r, 0, stmt);
    }

    r.report_unknown_names();
    r.report_unused_imports();

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
/// later `def x` or `class x` collides with an earlier `let x`.
fn collect_top_level(r: &mut Resolver, scope: ScopeId, body: &[Stmt]) {
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
                declare_target(r, scope, &a.target, default_val, a.mutability);
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
                let span = find_def_name_span(
                    r.source,
                    c.range.start().to_usize(),
                    "class ",
                    c.name.as_str(),
                );
                r.declare(
                    scope,
                    c.name.as_str(),
                    BindingKind::Class,
                    Mutability::Mut,
                    span,
                );
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

fn declare_target(
    r: &mut Resolver,
    scope: ScopeId,
    target: &Expr,
    default_val: bool,
    ast_mutability: Option<ast::Mutability>,
) {
    if let Expr::Name(n) = target {
        // When no explicit `let`/`mut` keyword is present in the AST,
        // treat a bare assignment as a rebinding of any existing binding
        // (taking its mutability) rather than a fresh declaration. Only
        // the *first* bareword assignment in a module scope defaults to
        // `let`; later bare assignments inherit the existing binding's
        // mutability.
        let existing_mut = r.lookup_local(scope, n.id.as_str()).map(|b| b.mutability);
        let mutability = match ast_mutability {
            Some(ast::Mutability::Let) => Mutability::Let,
            Some(ast::Mutability::Mut) => Mutability::Mut,
            None => existing_mut.unwrap_or(if default_val {
                Mutability::Let
            } else {
                Mutability::Mut
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
/// Convert an AST `TextRange` to the (start, end) byte tuple used by
/// `Scope::span` and binding spans.
fn range_to_span(range: TextRange) -> (usize, usize) {
    (range.start().to_usize(), range.end().to_usize())
}

fn walk_stmt(r: &mut Resolver, scope: ScopeId, stmt: &Stmt) {
    match stmt {
        Stmt::FunctionDef(f) => {
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
                if let Expr::Name(_) = t {
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
            if let Expr::Name(_) = a.target.as_ref() {
                let default_val = r.scopes[scope].kind == ScopeKind::Module;
                declare_target(r, scope, &a.target, default_val, a.mutability);
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
            // Loop target introduces a binding.
            if let Expr::Name(n) = f.target.as_ref() {
                let span = (
                    n.range.start().to_usize(),
                    n.range.start().to_usize() + n.id.as_str().len(),
                );
                r.declare(
                    scope,
                    n.id.as_str(),
                    BindingKind::Loop,
                    Mutability::Mut,
                    span,
                );
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
                        r.declare(
                            scope,
                            n.id.as_str(),
                            BindingKind::Loop,
                            Mutability::Mut,
                            span,
                        );
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
            walk_argument_annotations(r, cls_scope, &f.parameters);
            if let Some(ret) = &f.returns {
                walk_expr(r, cls_scope, ret);
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
        let prep = preprocess(src);
        let module = tyc_syntax::parse_module(&prep.python_source)
            .unwrap()
            .into_syntax();
        resolve_module("<test>".to_owned(), &prep.python_source, &module)
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
    fn unknown_name_errors() {
        let (_m, d) = resolve("y = z + 1\n");
        assert!(d.has_errors());
        let msg = format!("{}", d.errors()[0]);
        assert!(msg.contains("cannot find 'z'"), "got {}", msg);
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
