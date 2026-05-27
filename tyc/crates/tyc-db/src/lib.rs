//! Salsa incremental database for Typhon.
//!
//! Phase 1 establishes the database scaffolding: source files are stored
//! as salsa inputs, and two tracked queries — `preprocessed_text` and
//! `module_decl_names` — demonstrate the pattern. The full type-checking
//! pipeline is exposed via [`check_file`], which uses the salsa db
//! internally and runs the heavier passes that don't yet have
//! `salsa::Update`-compatible outputs.
//!
//! Later phases will migrate more passes (resolve, type-check) into
//! tracked queries as their output types acquire `salsa::Update`.

use std::sync::Arc;

use tyc_diagnostics::{Diagnostics, TycError};
use tyc_resolve::{resolve_module_with, LazyImportRemap, ResolveOptions, ResolvedModule};
use tyc_syntax::{
    parse_module,
    preprocess::{
        expand_gather_blocks, expand_go_calls, expand_inline_question_ops, expand_lazy_lets,
        expand_multiline_guards, expand_pipes, expand_question_ops, expand_typed_let_unpack,
        expand_with_chains, line_byte_starts, preprocess, validate_extend_usage,
        validate_lazy_usage, validate_question_ops, PreprocessResult,
    },
};
use tyc_types::{
    check_module_with, check_module_with_imports, extract_module_shapes, ExternalShapes,
};

/// Re-export so downstream crates (CLI, LSP) can name the type
/// without depending on `tyc-types` directly.
pub use tyc_types::ModuleShapes;

/// A source file held by the database — identified by path, with mutable
/// text content. Changing `text` invalidates every query that derives
/// from this input.
#[salsa::input]
pub struct SourceFile {
    #[returns(ref)]
    pub path: String,
    #[returns(ref)]
    pub text: String,
}

/// Tracked query: the preprocessed (Python-compatible) text of a file.
///
/// This is the "parse-prepare" step: it strips Typhon-specific line-prefix
/// keywords (`let`/`mut`, `model`, `interface`, etc.) and rewrites `T?` to
/// `T | None`. Salsa caches the result, so an editor edit that doesn't change
/// the file's text content (e.g. saving with no edits) avoids re-running the
/// preprocess pass.
#[salsa::tracked]
pub fn preprocessed_text(db: &dyn salsa::Database, file: SourceFile) -> String {
    // Delegate to the full-result query so the expand+preprocess work is
    // shared with `resolved_module` and the check pipeline. Salsa caches
    // both queries independently: if only the text query is consumed,
    // the per-revision cost is still one preprocess pass.
    preprocessed_full(db, file).python_source.clone()
}

/// Newtype wrapper around `Arc<PreprocessResult>` so we can satisfy
/// `salsa::Update` without violating the orphan rule. Mirrors
/// [`ArcResolvedModule`] / [`ArcDiagnostics`] — pointer equality is the
/// equivalence relation, which is conservative but sound (Salsa only
/// calls `maybe_update` after the query body has re-run).
#[derive(Clone)]
pub struct ArcPreprocessResult(pub Arc<PreprocessResult>);

impl PartialEq for ArcPreprocessResult {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ArcPreprocessResult {}

impl std::ops::Deref for ArcPreprocessResult {
    type Target = PreprocessResult;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// SAFETY: same argument as for `ArcResolvedModule`.
unsafe impl salsa::Update for ArcPreprocessResult {
    unsafe fn maybe_update(old_pointer: *mut Self, new_value: Self) -> bool {
        if Arc::ptr_eq(&(*old_pointer).0, &new_value.0) {
            false
        } else {
            *old_pointer = new_value;
            true
        }
    }
}

/// Tracked query: run sugar-expansion + the preprocessor and cache the
/// full [`PreprocessResult`].
///
/// Both [`preprocessed_text`] and [`resolved_module`] need the same
/// expand-then-preprocess pipeline; sharing it through this query means
/// each source-text change runs the work exactly once instead of three
/// times (preprocessed_text, resolved_module, and the check pipeline).
#[salsa::tracked]
pub fn preprocessed_full(db: &dyn salsa::Database, file: SourceFile) -> ArcPreprocessResult {
    let text = file.text(db);
    let expanded = expand_question_ops(&expand_inline_question_ops(&expand_pipes(
        &expand_with_chains(&expand_go_calls(&expand_gather_blocks(
            &expand_multiline_guards(&expand_lazy_lets(&expand_typed_let_unpack(text))),
        ))),
    )));
    ArcPreprocessResult(Arc::new(preprocess(&expanded)))
}

/// Tracked query: the names declared at the top level of the module.
///
/// This is a cheap proxy for "module resolution": it parses the
/// preprocessed source and returns the list of top-level binding names.
/// The full [`ResolvedModule`](tyc_resolve::ResolvedModule) isn't yet
/// `salsa::Update`-friendly, so this is the slice of the resolve step
/// that's salsa-cacheable today.
#[salsa::tracked]
pub fn module_decl_names(db: &dyn salsa::Database, file: SourceFile) -> Vec<String> {
    // Reuse the cached resolved module so a hover / completion path
    // doesn't trigger an independent parse+resolve cycle.
    resolved_module(db, file)
        .module_scope()
        .bindings
        .iter()
        .map(|b| b.name.clone())
        .collect()
}

/// Newtype wrapper around `(Arc<ResolvedModule>, Arc<Diagnostics>)` so we can implement
/// `salsa::Update` for it without violating the orphan rule.
///
/// Salsa requires the return type of a `#[salsa::tracked]` query to implement
/// `Update`.  `ResolvedModule` contains `Vec`s of structs that don't implement
/// `PartialEq`, so we use pointer comparison.  Salsa only calls `maybe_update`
/// after the query body has already re-run (i.e. when an input changed), so
/// the conservative "always-changed" strategy is correct.
///
/// This wrapper now holds both the resolved module and the diagnostics generated
/// during resolution as separate Arcs, eliminating the need to re-run `resolve_module_with`
/// just to collect diagnostics while preserving the ability to extract the ResolvedModule Arc.
#[derive(Clone)]
pub struct ArcResolvedModule(Arc<ResolvedModule>, Arc<Diagnostics>);

impl ArcResolvedModule {
    /// Construct a new `ArcResolvedModule` from a resolved module and diagnostics.
    pub fn new(resolved: Arc<ResolvedModule>, diagnostics: Arc<Diagnostics>) -> Self {
        Self(resolved, diagnostics)
    }

    /// Access the resolved module.
    pub fn resolved(&self) -> &ResolvedModule {
        &self.0
    }

    /// Access the resolution diagnostics.
    pub fn diagnostics(&self) -> &Diagnostics {
        &self.1
    }

    /// Get the Arc<ResolvedModule> for compatibility.
    pub fn resolved_arc(&self) -> Arc<ResolvedModule> {
        Arc::clone(&self.0)
    }

    /// Consume self and return the inner Arc<ResolvedModule> by move,
    /// avoiding extra refcount operations.
    pub fn into_resolved_arc(self) -> Arc<ResolvedModule> {
        self.0
    }

    /// Get a reference to the diagnostics Arc.
    pub fn diagnostics_arc(&self) -> &Arc<Diagnostics> {
        &self.1
    }
}

impl PartialEq for ArcResolvedModule {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) && Arc::ptr_eq(&self.1, &other.1)
    }
}

impl Eq for ArcResolvedModule {}

impl std::ops::Deref for ArcResolvedModule {
    type Target = ResolvedModule;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// SAFETY: `old_pointer` is a valid, aligned, live pointer to an `ArcResolvedModule`
// managed by Salsa.  The assignment `*old_pointer = new_value` drops the previous
// Arcs (decrementing their refcounts) before storing the new ones, which is correct.
// Pointer equality is used as a conservative proxy for value equality.
unsafe impl salsa::Update for ArcResolvedModule {
    unsafe fn maybe_update(old_pointer: *mut Self, new_value: Self) -> bool {
        if Arc::ptr_eq(&(*old_pointer).0, &new_value.0)
            && Arc::ptr_eq(&(*old_pointer).1, &new_value.1)
        {
            false
        } else {
            *old_pointer = new_value;
            true
        }
    }
}

/// Newtype wrapper around `Arc<Diagnostics>` for use as a `#[salsa::tracked]`
/// query return type.
///
/// Mirrors the design of [`ArcResolvedModule`]: Salsa requires `Update` on
/// return types, and `Diagnostics` does not implement it.  Pointer equality
/// is used as a conservative proxy — every re-run allocates a fresh `Arc`, so
/// this reports "changed" on every input change, which is sound.
#[derive(Clone)]
pub struct ArcDiagnostics(pub Arc<Diagnostics>);

impl ArcDiagnostics {
    fn new(d: Diagnostics) -> Self {
        Self(Arc::new(d))
    }
}

impl PartialEq for ArcDiagnostics {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ArcDiagnostics {}

impl std::ops::Deref for ArcDiagnostics {
    type Target = Diagnostics;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// SAFETY: same argument as for `ArcResolvedModule`.
unsafe impl salsa::Update for ArcDiagnostics {
    unsafe fn maybe_update(old_pointer: *mut Self, new_value: Self) -> bool {
        if Arc::ptr_eq(&(*old_pointer).0, &new_value.0) {
            false
        } else {
            *old_pointer = new_value;
            true
        }
    }
}

/// Salsa-tracked query: run the full check pipeline for a file and return
/// the cached [`Diagnostics`].
///
/// Salsa re-evaluates this only when `file.text` changes — so subsequent
/// calls on an unchanged file are instant cache hits.  This makes the LSP
/// path (`check_source_file`) incremental: a `did_open` event populates the
/// cache; a `hover` or `definition` request that triggers a re-check on the
/// same unchanged source returns immediately.
///
/// The public [`check_source_file`] function unwraps the `Arc` so callers
/// continue to receive a plain [`Diagnostics`] value.
#[salsa::tracked]
fn check_diagnostics(db: &dyn salsa::Database, file: SourceFile) -> ArcDiagnostics {
    let path = file.path(db).clone();
    let text = file.text(db).clone();
    ArcDiagnostics::new(check_impl(&path, &text))
}

/// Tracked query: parse and resolve the preprocessed source of a file.
///
/// Salsa re-evaluates this only when `preprocessed_text` changes, so LSP
/// hover and go-to-definition handlers can call it directly instead of
/// maintaining a separate `HashMap` cache.  The resolver runs once per text
/// revision, and subsequent calls within the same revision are cache hits.
///
/// Returns an [`ArcResolvedModule`] (a thin newtype around
/// `Arc<ResolvedModule>`) so the `salsa::Update` impl can satisfy the orphan
/// rule.  Callers can deref directly or clone the inner `Arc` via `.0`.
#[salsa::tracked]
pub fn resolved_module(db: &dyn salsa::Database, file: SourceFile) -> ArcResolvedModule {
    let raw_text = file.text(db).clone();
    let path = file.path(db).clone();
    // Pull the cached preprocess result instead of running the sugar
    // pipeline again. After the first consumer in a revision triggers
    // `preprocessed_full`, subsequent calls (this query, the type-check
    // pipeline, the LSP hover path) are cache hits.
    let prep = preprocessed_full(db, file);
    let lazy_import_remaps = build_lazy_import_remaps(&raw_text, &prep.lazy_imports);
    let options = ResolveOptions {
        raw_class_byte_starts: line_byte_starts(&prep.python_source, &prep.raw_class_lines),
        lazy_import_remaps,
        original_source: Some(raw_text.clone()),
    };
    match parse_module(&prep.python_source) {
        Ok(parsed) => {
            let module = parsed.into_syntax();
            let (resolved, diags) =
                resolve_module_with(path, &prep.python_source, &module, options);
            ArcResolvedModule::new(Arc::new(resolved), Arc::new(diags))
        }
        Err(_) => ArcResolvedModule::new(
            Arc::new(ResolvedModule::default()),
            Arc::new(Diagnostics::new()),
        ),
    }
}

/// Convert preprocessor `lazy_imports` metadata into [`LazyImportRemap`]s
/// the resolver can consume. The preprocessor records each
/// `lazy import ALIAS = MODULE` statement's line index in the
/// *post-sugar* source (after multi-line-guard and other line-drifting
/// passes have run), so we cannot use that index directly into the
/// original Typhon source — a guard expansion that added lines above
/// would offset every subsequent lazy-import line.
///
/// Instead, walk the original source independently for `lazy import`
/// lines (which are never moved or removed by sugar passes — they only
/// appear at module level), then pair them with `lazy_imports` in
/// source order. The resolver still keys on `line_index` from the
/// preprocessed source (matching the binding's span) but the offset
/// it surfaces points at the alias in the original (FINDINGS #15).
fn build_lazy_import_remaps(
    original_source: &str,
    lazy_imports: &[tyc_syntax::preprocess::LazyImport],
) -> Vec<LazyImportRemap> {
    if lazy_imports.is_empty() {
        return Vec::new();
    }
    let original_aliases = collect_original_lazy_import_alias_spans(original_source);
    // Pair by source order. Sugar passes don't add, remove, or
    // reorder `lazy import` lines, so the nth lazy import in the
    // original is the nth lazy import in the preprocessed source.
    // Optionally verify the alias names match as a sanity check;
    // a mismatch (which would mean a sugar pass started producing
    // synthetic lazy imports) silently drops that remap so the
    // user gets the preprocessed-source fallback instead of a
    // mis-anchored diagnostic.
    let mut out = Vec::with_capacity(lazy_imports.len());
    for (i, li) in lazy_imports.iter().enumerate() {
        let Some(original) = original_aliases.get(i) else {
            continue;
        };
        if original.alias != li.alias {
            continue;
        }
        out.push(LazyImportRemap {
            line_index: li.line_index,
            original_alias_offset: original.offset,
            original_alias_length: original.length,
        });
    }
    out
}

/// One `lazy import ALIAS = MODULE` declaration as seen in the original
/// Typhon source, with the byte offset and length of the ALIAS token.
/// Internal to [`build_lazy_import_remaps`].
struct OriginalLazyAlias {
    alias: String,
    offset: usize,
    length: usize,
}

/// Walk `source` line-by-line and return every `lazy import ALIAS =
/// MODULE` declaration in source order. Mirrors the preprocessor's
/// recognition (only module-level — indent 0 — with the literal
/// `lazy import ` prefix), but operates on the *original* (pre-sugar)
/// text so the alias offsets remain valid after upstream line-drifting
/// passes like `expand_multiline_guards`.
fn collect_original_lazy_import_alias_spans(source: &str) -> Vec<OriginalLazyAlias> {
    let prefix = "lazy import ";
    let mut out = Vec::new();
    let mut line_start = 0usize;
    for (line_end, byte) in source
        .bytes()
        .enumerate()
        .map(|(i, b)| (i + 1, b))
        .filter(|&(_, b)| b == b'\n')
        .chain(std::iter::once((source.len() + 1, 0u8)))
    {
        // `line_end` is one past the `\n` (or one past EOF for the
        // synthetic terminator). Slice up to it minus the newline.
        let end_excl = line_end.saturating_sub(1).min(source.len());
        let line = &source[line_start..end_excl];
        // Module-level lazy imports start at indent 0. Indented `lazy`
        // expressions are left alone by the preprocessor, so we
        // mirror that here.
        if let Some(after_kw) = line.strip_prefix(prefix) {
            let extra_ws = after_kw
                .bytes()
                .take_while(|&b| b == b' ' || b == b'\t')
                .count();
            let alias_start_in_line = prefix.len() + extra_ws;
            let after_alias = &line[alias_start_in_line..];
            let alias_len = after_alias
                .bytes()
                .take_while(|&b| b.is_ascii_alphanumeric() || b == b'_')
                .count();
            if alias_len > 0 {
                let alias = after_alias[..alias_len].to_owned();
                out.push(OriginalLazyAlias {
                    alias,
                    offset: line_start + alias_start_in_line,
                    length: alias_len,
                });
            }
        }
        line_start = line_end;
        // Suppress unused-variable warning on `byte` (only used to
        // gate the iter chain above).
        let _ = byte;
    }
    out
}

/// Convenience alias — extract the inner `Arc<ResolvedModule>` from a
/// `resolved_module` query result. Returns the same Arc on repeated calls
/// for the same file (pointer equality), so LSP caching tests pass.
pub fn resolved_module_arc(db: &dyn salsa::Database, file: SourceFile) -> Arc<ResolvedModule> {
    resolved_module(db, file).into_resolved_arc()
}

/// The Typhon database — concrete carrier of salsa state.
#[salsa::db]
#[derive(Clone, Default)]
pub struct TycDatabase {
    storage: salsa::Storage<Self>,
}

#[salsa::db]
impl salsa::Database for TycDatabase {}

impl TycDatabase {
    pub fn new() -> Self {
        Self::default()
    }
}

/// End-to-end check pipeline for a single file. Returns parse, resolve,
/// and type-check diagnostics merged in source order (parse first).
///
/// Run the full check pipeline for `(path, text)` and return diagnostics.
///
/// Creates a temporary [`SourceFile`] entry in `db` so the Salsa-tracked
/// `check_diagnostics` query can cache the result.  Subsequent calls with the
/// same path and text are instant cache hits; calls after a text change
/// invalidate the cache and re-run the pipeline.
pub fn check_file(db: &mut TycDatabase, path: String, text: String) -> Diagnostics {
    let file = SourceFile::new(db, path, text);
    (*check_diagnostics(db, file).0).clone()
}

/// Like [`check_file`] but uses a caller-supplied [`SourceFile`] handle.
///
/// The handle must already exist in `db` (created via [`SourceFile::new`] or
/// updated via `source_file.set_text(&mut db).to(text)`).  The LSP uses this
/// variant so it can retain the handle across `did_open`/`did_change` events
/// and then call [`preprocessed_text`] from hover/definition handlers.
///
/// The full check pipeline is now Salsa-tracked via [`check_diagnostics`]:
/// repeated calls on an unchanged source file return the cached result
/// immediately, making incremental LSP re-checks near-zero cost.
pub fn check_source_file(db: &mut TycDatabase, source_file: SourceFile) -> Diagnostics {
    (*check_diagnostics(db, source_file).0).clone()
}

/// Extract the publicly-visible class / function shapes from a Typhon
/// source file without running the resolver or type checker. Used by
/// the CLI and LSP backend to build a project-wide shape registry
/// before the per-file check loop, so cross-module constructor /
/// method arity validation has the data it needs.
///
/// Runs the same preprocess + parse front-end as [`check_impl`], but
/// stops there. Returns an empty [`ModuleShapes`] on any parse error
/// — the real diagnostic surfaces when the file is checked for real.
pub fn extract_shapes_for_path(_path: &str, text: &str) -> ModuleShapes {
    let expanded = expand_question_ops(&expand_inline_question_ops(&expand_pipes(
        &expand_with_chains(&expand_go_calls(&expand_gather_blocks(
            &expand_multiline_guards(&expand_lazy_lets(&expand_typed_let_unpack(text))),
        ))),
    )));
    let prep = preprocess(&expanded);
    let module = match parse_module(&prep.python_source) {
        Ok(p) => p.into_syntax(),
        Err(_) => return ModuleShapes::default(),
    };
    extract_module_shapes(&module)
}

/// Newtype wrapper around `Arc<ModuleShapes>` so the Salsa-tracked
/// `module_shapes_query` can satisfy `salsa::Update`. Mirrors
/// [`ArcResolvedModule`] / [`ArcDiagnostics`] for the same orphan-rule
/// reason. Pointer-equality is the equivalence relation (a fresh
/// extraction allocates a new `Arc`, so "different `Arc` = changed").
#[derive(Clone)]
pub struct ArcModuleShapes(pub Arc<ModuleShapes>);

impl PartialEq for ArcModuleShapes {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ArcModuleShapes {}

impl std::ops::Deref for ArcModuleShapes {
    type Target = ModuleShapes;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// SAFETY: same argument as for `ArcResolvedModule`.
unsafe impl salsa::Update for ArcModuleShapes {
    unsafe fn maybe_update(old_pointer: *mut Self, new_value: Self) -> bool {
        if Arc::ptr_eq(&(*old_pointer).0, &new_value.0) {
            false
        } else {
            *old_pointer = new_value;
            true
        }
    }
}

/// Salsa-tracked variant of [`extract_shapes_for_path`]. The LSP
/// backend keeps a `HashMap<dotted_name, SourceFile>` per project
/// root and queries this for each file — Salsa re-runs the
/// extraction only on the file whose text changed, so a keystroke
/// in `src/main.ty` doesn't re-parse `src/clients.ty`.
///
/// The result is wrapped in [`ArcModuleShapes`]; callers typically
/// unwrap via `.0.clone()` to drop the wrapper.
#[salsa::tracked]
pub fn module_shapes_query(db: &dyn salsa::Database, file: SourceFile) -> ArcModuleShapes {
    let text = file.text(db).clone();
    let shapes = extract_shapes_for_path(&file.path(db).clone(), &text);
    ArcModuleShapes(Arc::new(shapes))
}

/// Variant of [`check_file`] that consults a pre-built project-wide
/// shape registry so cross-module constructor / method arity checks
/// fire when an imported class is called.
///
/// The caller (typically the `tyc check` / `tyc build` driver or the
/// LSP backend) walks the project, populates `shapes_by_module` with
/// every dotted module name → [`ModuleShapes`] pairing, and then
/// invokes this for each file in turn.
///
/// Unlike [`check_file`], this entry point does NOT go through the
/// Salsa-cached `check_diagnostics` query — it threads the
/// per-invocation `shapes_by_module` parameter through the checker,
/// which couldn't be represented as a Salsa input without dragging
/// the whole project's source state into the cache.
pub fn check_file_with_imports(
    db: &mut TycDatabase,
    path: String,
    text: String,
    shapes_by_module: &std::sync::Arc<std::collections::HashMap<String, ModuleShapes>>,
) -> Diagnostics {
    let file = SourceFile::new(db, path, text);
    check_source_file_with_imports(db, file, shapes_by_module)
}

/// Cross-module variant of [`check_source_file`] that consults a pre-
/// built project-wide shape registry.
///
/// Like [`check_source_file`] this takes a [`SourceFile`] handle —
/// callers (LSP, watch-mode build drivers) hold one per file across
/// invocations and update its `text` via `set_text`. The Salsa-tracked
/// `preprocessed_full` and `resolved_module` queries make the parse +
/// resolve cycle a cache hit when the file's text hasn't changed; only
/// the type-check (which depends on the per-invocation
/// `shapes_by_module` registry, not a Salsa input) actually runs again.
pub fn check_source_file_with_imports(
    db: &mut TycDatabase,
    file: SourceFile,
    shapes_by_module: &std::sync::Arc<std::collections::HashMap<String, ModuleShapes>>,
) -> Diagnostics {
    let path = file.path(db).clone();
    let text = file.text(db).clone();

    let mut diags = Diagnostics::new();

    // The validation passes run on the raw source — cheap regex-ish
    // scans, no AST. We keep them outside the cached pipeline because
    // their diagnostics depend on column/offset detail that the
    // preprocess pass discards.
    for err in validate_question_ops(&text) {
        diags.push_error(TycError::invalid_question_op(
            err.message,
            &path,
            &text,
            err.offset,
            1,
        ));
    }
    for err in validate_lazy_usage(&text) {
        diags.push_error(TycError::lazy_usage(
            err.message,
            &path,
            &text,
            err.offset,
            4,
        ));
    }
    for err in validate_extend_usage(&text) {
        diags.push_error(TycError::extend_builtin(
            err.message,
            &path,
            &text,
            err.offset,
            6,
        ));
    }
    if diags.has_errors() {
        return diags;
    }

    // Tracked queries: this is the cache win — these two return the
    // same Arc when `file.text` hasn't changed since the last call.
    let prep = preprocessed_full(db, file);
    let resolved_arc = resolved_module(db, file);

    // Parse the module body for the type checker. The parse output
    // isn't cached as its own Salsa value because the `ModModule`
    // AST is huge and doesn't implement `salsa::Update`; instead we
    // re-parse from the cached preprocessed source. The cost is one
    // O(file) parse per check call; the bigger win — skipping the
    // expand + preprocess pipeline — is already realised by the
    // tracked `preprocessed_full` above.
    let module = match parse_module(&prep.python_source) {
        Ok(p) => p.into_syntax(),
        Err(e) => {
            diags.push_error(TycError::parse(
                path,
                prep.python_source.clone(),
                e.to_string(),
                usize::from(e.location.start()),
            ));
            return diags;
        }
    };

    // B34: inline comptime values into the AST so the type-checker
    // sees `comptime let T: type = int` as a `type T = int` alias
    // declaration. Without this, `T` resolves as a distinct nominal
    // class and `def f(x: T)` rejects `int` arguments. Matches the
    // same substitution `tyc build` and `tyc run` apply.
    let (comptime_values, _comptime_diags) =
        tyc_analyse::evaluate_comptime_with_functions(
            &module,
            &prep.comptime_bindings,
            &prep.comptime_functions,
        );
    let module = tyc_analyse::substitute_comptime_literals(
        module,
        &comptime_values,
        &prep.comptime_functions,
    );

    // Collect resolve diagnostics from the cached query. The
    // `resolved_module` query now stores both the resolved bindings
    // and the diagnostics from resolution, so we don't need to re-run
    // `resolve_module_with` here.
    diags.extend(resolved_arc.diagnostics().clone());

    let external = build_external_shapes(&resolved_arc, shapes_by_module);
    let type_diags = check_module_with_imports(
        path,
        &prep.python_source,
        &resolved_arc,
        &module,
        &prep.unsafe_lines,
        &prep.frozen_class_lines,
        Some(&external),
    );
    diags.extend(type_diags);

    diags
}

/// Walk the resolved module's bindings, pick out every import, and
/// look its source module up in the project registry. Builds the
/// [`ExternalShapes`] snapshot that
/// [`tyc_types::check_module_with_imports`] consumes.
///
/// Both import shapes are now wired:
///
/// - `from M import X` (with or without `as Y`) → the local name `X`
///   (or `Y`) gets the class shape / function arity that module `M`
///   exports for `X`. Flat by-name seeding so the local name lands
///   as `Type::Class("X")` and constructor / arity checks fire
///   transparently.
/// - `import M` / `import M as N` → the local name binds to
///   `Type::Module("M")`; attribute access (`N.SomeClass(...)`)
///   resolves through `by_module` and registers the foreign class
///   shape on-demand so the constructor call site arity-checks
///   normally. The full `shapes_by_module` registry is cloned into
///   `by_module` so the checker can satisfy any attribute access on
///   any imported module without further callbacks.
fn build_external_shapes(
    resolved: &ResolvedModule,
    shapes_by_module: &std::sync::Arc<std::collections::HashMap<String, ModuleShapes>>,
) -> ExternalShapes {
    // Just bump the refcount — the caller (`tyc check` / `tyc
    // build` / the LSP) constructs the registry once per
    // invocation and the per-file `ExternalShapes` snapshots
    // share it. FINDINGS — copilot review of v0.2.0.
    let mut external = ExternalShapes {
        by_module: std::sync::Arc::clone(shapes_by_module),
        ..ExternalShapes::default()
    };
    // Module-scope bindings live in scope 0.
    let bindings = &resolved.scopes[0].bindings;
    // Per-source-module reverse map: original exported name → local
    // import name, so a `from foo import A as MyA` translates the
    // variants list of an imported sealed union into the same local
    // names the consumer's `class_shapes` is keyed under.
    let mut local_by_module: std::collections::HashMap<
        String,
        std::collections::HashMap<String, String>,
    > = std::collections::HashMap::new();
    for b in bindings {
        if let Some(info) = &b.import_info {
            if let Some(member) = info.member.as_ref() {
                local_by_module
                    .entry(info.module.clone())
                    .or_default()
                    .insert(member.clone(), b.name.clone());
            }
        }
    }
    for b in bindings {
        let Some(info) = &b.import_info else { continue };
        let Some(member) = info.member.as_ref() else {
            // Bare `import M as N` — record the alias mapping so the
            // checker can render `N.SomeClass(...)` via the module
            // registry. The shape lookup at attribute-access time
            // uses `info.module` (the original dotted name), not the
            // local alias.
            external
                .bare_imports
                .insert(b.name.clone(), info.module.clone());
            continue;
        };
        let Some(module_shapes) = shapes_by_module.get(&info.module) else {
            continue;
        };
        if let Some(shape) = module_shapes.class_shapes.get(member) {
            external.class_shapes.insert(b.name.clone(), shape.clone());
            if let Some(tps) = module_shapes.class_type_params.get(member) {
                external
                    .class_type_params
                    .insert(b.name.clone(), tps.clone());
            }
            // If the foreign module declared `Foo` as an interface
            // (Protocol-shaped), record that fact — together with
            // the source's `@runtime_checkable` opt-in — under the
            // local import name so cross-module structural
            // conformance matches the in-module checker and
            // `isinstance(x, ImportedInterface)` is allowed when
            // the source author opted in.
            if let Some(runtime_checkable) = module_shapes.interfaces.get(member) {
                external
                    .interfaces
                    .insert(b.name.clone(), *runtime_checkable);
            }
        } else if let Some(arity) = module_shapes.function_arities.get(member) {
            external
                .function_arities
                .insert(b.name.clone(), arity.clone());
        } else if let Some(variants) = module_shapes.sealed_unions.get(member) {
            // Sealed-union alias imported by name. Re-key under the
            // local import name *and* translate each variant name
            // through the per-module local-name map so
            // `from foo import A as MyA, Event` is seen as
            // `Event = MyA | …` by the consumer's checker.
            let remap = local_by_module.get(&info.module);
            let mapped: Vec<String> = variants
                .iter()
                .map(|v| {
                    remap
                        .and_then(|m| m.get(v))
                        .cloned()
                        .unwrap_or_else(|| v.clone())
                })
                .collect();
            external.sealed_unions.insert(b.name.clone(), mapped);
        }
    }
    // R1-#1 follow-up: variant→union upcasts need the union's variant
    // table at the consumer site even when the union NAME wasn't
    // imported. The factory-cleanup sweep over the apps removed the
    // `make_event` factories that previously wrapped construction in
    // a `-> SchedulerEvent` return type, exposing call sites that
    // pass a bare variant constructor (`emit(WorkerStarted(...))`)
    // into a function whose formal parameter is `SchedulerEvent`.
    // Without this seeding, `c.sealed_unions["SchedulerEvent"]` is
    // empty and the upcast fails — even though the formal's union
    // name is visible to the consumer's checker via the function's
    // imported signature.
    //
    // Walk every sealed union declared in every imported module: if
    // ANY of its variants is imported into the consumer scope,
    // populate `external.sealed_unions[union_name]` with the union's
    // variant list (re-keyed through the per-module local-name map
    // for the imported variants, source names as fallback). The
    // union name itself is the source-module name (e.g.
    // `SchedulerEvent`), matching the formal parameter type the
    // checker sees on the cross-module function signature.
    let mut modules_touched: std::collections::HashSet<String> = std::collections::HashSet::new();
    for b in bindings {
        let Some(info) = &b.import_info else { continue };
        if info.member.is_none() {
            continue;
        }
        if !modules_touched.insert(info.module.clone()) {
            continue;
        }
        let Some(module_shapes) = shapes_by_module.get(&info.module) else {
            continue;
        };
        let remap = local_by_module.get(&info.module);
        for (union_name, variants) in &module_shapes.sealed_unions {
            // Skip if already populated (handled above when the union
            // name itself was imported).
            if external.sealed_unions.contains_key(union_name) {
                continue;
            }
            // Only seed when at least one variant is imported here —
            // otherwise the variant→union upcast wouldn't be reachable
            // anyway and the extra entry would be dead weight.
            let any_variant_imported = variants
                .iter()
                .any(|v| remap.map(|m| m.contains_key(v)).unwrap_or(false));
            if !any_variant_imported {
                continue;
            }
            let mapped: Vec<String> = variants
                .iter()
                .map(|v| {
                    remap
                        .and_then(|m| m.get(v))
                        .cloned()
                        .unwrap_or_else(|| v.clone())
                })
                .collect();
            external.sealed_unions.insert(union_name.clone(), mapped);
        }
    }
    external
}

/// Shared check implementation used by [`check_diagnostics`] (and transitively
/// by [`check_file`] and [`check_source_file`]).
fn check_impl(path: &str, text: &str) -> Diagnostics {
    // The resolver and type-checker need the full PreprocessResult (including
    // `stripped` and `optionals` metadata), which doesn't yet implement
    // `salsa::Update` — so we run preprocess directly here. The
    // `preprocessed_text` salsa query above remains the cached entry point
    // for callers (e.g. the LSP hover handler) that only need the
    // Python-compatible source string.
    let mut diags = Diagnostics::new();

    // Validate `?` operator context before expanding it.  This runs on the
    // original Typhon source so it can reason about indentation-based scopes.
    // Return early on any errors: invalid `?` usage causes `expand_question_ops`
    // to inject `return` at top level, which would produce a cascading parse
    // error that obscures the real problem.
    for err in validate_question_ops(text) {
        diags.push_error(TycError::invalid_question_op(
            err.message,
            path,
            text,
            err.offset,
            1,
        ));
    }
    // Reject unsupported `lazy from … import …` constructs early so the
    // downstream parser doesn't try to give a misleading diagnostic.
    for err in validate_lazy_usage(text) {
        diags.push_error(TycError::lazy_usage(
            err.message,
            path,
            text,
            err.offset,
            4, // length of "lazy"
        ));
    }
    // Reject `extend BUILTIN:` declarations.  Python's built-in types cannot
    // be modified at runtime, so the silent drop performed by the impl-merge
    // desugar pass would surprise the user.
    for err in validate_extend_usage(text) {
        diags.push_error(TycError::extend_builtin(
            err.message,
            path,
            text,
            err.offset,
            6, // length of "extend"
        ));
    }
    if diags.has_errors() {
        return diags;
    }

    // Apply Typhon sugar expansion in order before preprocessing so the
    // Python parser sees valid Python.  `tyc fmt` skips these expansions to
    // preserve Typhon syntax in the formatter's round trip.
    let expanded = expand_question_ops(&expand_inline_question_ops(&expand_pipes(
        &expand_with_chains(&expand_go_calls(&expand_gather_blocks(
            &expand_multiline_guards(&expand_lazy_lets(&expand_typed_let_unpack(text))),
        ))),
    )));
    let prep = preprocess(&expanded);

    let module = match parse_module(&prep.python_source) {
        Ok(p) => p.into_syntax(),
        Err(e) => {
            diags.push_error(TycError::parse(
                path.to_owned(),
                prep.python_source,
                e.to_string(),
                usize::from(e.location.start()),
            ));
            return diags;
        }
    };

    let resolve_options = ResolveOptions {
        raw_class_byte_starts: line_byte_starts(&prep.python_source, &prep.raw_class_lines),
        lazy_import_remaps: build_lazy_import_remaps(text, &prep.lazy_imports),
        original_source: Some(text.to_owned()),
    };
    let (resolved, resolve_diags) = resolve_module_with(
        path.to_owned(),
        &prep.python_source,
        &module,
        resolve_options,
    );
    diags.extend(resolve_diags);

    let type_diags = check_module_with(
        path.to_owned(),
        &prep.python_source,
        &resolved,
        &module,
        &prep.unsafe_lines,
        &prep.frozen_class_lines,
    );
    diags.extend(type_diags);

    diags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preprocessed_text_query_caches() {
        let db = TycDatabase::new();
        let file = SourceFile::new(&db, "<test>".to_owned(), "let x: int = 1\n".to_owned());
        let p1 = preprocessed_text(&db, file);
        let p2 = preprocessed_text(&db, file);
        assert_eq!(p1, "let x: int = 1\n");
        assert_eq!(p1, p2);
    }

    #[test]
    fn module_decl_names_query() {
        let db = TycDatabase::new();
        let file = SourceFile::new(
            &db,
            "<test>".to_owned(),
            "let x: int = 1\nmut y: int = 2\ndef f() -> None:\n    pass\n".to_owned(),
        );
        let names = module_decl_names(&db, file);
        assert!(names.contains(&"x".to_owned()));
        assert!(names.contains(&"y".to_owned()));
        assert!(names.contains(&"f".to_owned()));
    }

    // ── check_source_file ────────────────────────────────────────────────────

    #[test]
    fn check_source_file_clean_program() {
        let mut db = TycDatabase::new();
        let sf = SourceFile::new(&db, "<test>".to_owned(), "let x: int = 1\n".to_owned());
        let diags = check_source_file(&mut db, sf);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
    }

    #[test]
    fn check_source_file_reports_type_error() {
        let mut db = TycDatabase::new();
        let sf = SourceFile::new(&db, "<test>".to_owned(), "let x: int = \"hi\"\n".to_owned());
        let diags = check_source_file(&mut db, sf);
        assert!(diags.has_errors(), "should report type mismatch");
    }

    #[test]
    fn set_text_invalidates_preprocessed_text_cache() {
        use salsa::Setter;
        let mut db = TycDatabase::new();
        let sf = SourceFile::new(&db, "<test>".to_owned(), "let x: int = 1\n".to_owned());
        let first = preprocessed_text(&db, sf);
        assert_eq!(first, "let x: int = 1\n");
        // Update the file text — Salsa should invalidate the cached result.
        sf.set_text(&mut db)
            .to("let y: str = \"hello\"\n".to_owned());
        let second = preprocessed_text(&db, sf);
        assert_eq!(second, "let y: str = \"hello\"\n");
        assert_ne!(
            first, second,
            "cached result must be invalidated after set_text"
        );
    }

    #[test]
    fn check_source_file_after_set_text_uses_new_content() {
        use salsa::Setter;
        let mut db = TycDatabase::new();
        let sf = SourceFile::new(&db, "<test>".to_owned(), "let x: int = 1\n".to_owned());
        // First check: no errors.
        let diags1 = check_source_file(&mut db, sf);
        assert!(!diags1.has_errors(), "first check should pass");
        // Update text to introduce a type mismatch.
        sf.set_text(&mut db)
            .to("let x: int = \"oops\"\n".to_owned());
        let diags2 = check_source_file(&mut db, sf);
        assert!(
            diags2.has_errors(),
            "second check should fail after set_text"
        );
    }

    #[test]
    fn check_file_clean_program() {
        let mut db = TycDatabase::new();
        let diags = check_file(&mut db, "<test>".to_owned(), "let x: int = 1\n".to_owned());
        assert!(!diags.has_errors(), "{:?}", diags.errors());
    }

    #[test]
    fn check_file_reports_type_mismatch() {
        let mut db = TycDatabase::new();
        let diags = check_file(
            &mut db,
            "<test>".to_owned(),
            "let x: int = \"hi\"\n".to_owned(),
        );
        assert!(diags.has_errors());
    }

    #[test]
    fn check_file_unsafe_block_suppresses_type_errors() {
        // Inside an `unsafe:` block, type mismatches are suppressed so the
        // user can interface with untyped Python.  Identical code outside the
        // block remains an error (covered by check_file_reports_type_mismatch).
        let mut db = TycDatabase::new();
        let src = "\
unsafe:
    let x: int = \"hi\"
";
        let diags = check_file(&mut db, "<test>".to_owned(), src.to_owned());
        assert!(
            !diags.has_errors(),
            "unsafe block should suppress type errors; got {:?}",
            diags.errors()
        );
    }

    #[test]
    fn check_file_accepts_extend_on_builtin_str() {
        // `extend BUILTIN:` was previously a hard error.  As of the
        // extension-method-on-builtins work it is accepted: preprocess
        // lowers the block to a sentinel class that downstream passes
        // promote to free functions plus a call-site rewrite.  The type
        // checker should therefore see no diagnostics here.
        let mut db = TycDatabase::new();
        let src = "extend str:\n    def slug(self) -> str: return self\n";
        let diags = check_file(&mut db, "<test>".to_owned(), src.to_owned());
        assert!(
            !diags.has_errors(),
            "extend on a built-in type must no longer error; got {:?}",
            diags.errors()
        );
    }

    #[test]
    fn check_file_allows_extend_on_user_class() {
        let mut db = TycDatabase::new();
        let src = "\
class User:
    name: str

extend User:
    def greet(self) -> str: return self.name
";
        let diags = check_file(&mut db, "<test>".to_owned(), src.to_owned());
        assert!(
            !diags.has_errors(),
            "extend on a user class must be accepted; got {:?}",
            diags.errors()
        );
    }

    #[test]
    fn check_file_unsafe_block_does_not_leak_to_outer_scope() {
        // A type error on a line outside the `unsafe:` block must still be
        // reported even though another error occurs inside.
        let mut db = TycDatabase::new();
        let src = "\
let outer: int = \"oops\"
unsafe:
    let inner: int = \"hi\"
";
        let diags = check_file(&mut db, "<test>".to_owned(), src.to_owned());
        assert!(
            diags.has_errors(),
            "type error on outer line must still be reported"
        );
        // Exactly one error: the inner one is suppressed.
        assert_eq!(
            diags.errors().len(),
            1,
            "only the outer error should survive; got {:?}",
            diags.errors()
        );
    }

    #[test]
    fn check_file_reports_unknown_name() {
        let mut db = TycDatabase::new();
        let diags = check_file(&mut db, "<test>".to_owned(), "y = z\n".to_owned());
        assert!(diags.has_errors());
    }

    #[test]
    fn check_file_handles_scaffolded_program() {
        let src = "\
# myapp — entry point
#
# generated by `tyc init`

let greeting: str = \"Hello from Typhon!\"

def main() -> None:
    print(greeting)
";
        let mut db = TycDatabase::new();
        let diags = check_file(&mut db, "<test>".into(), src.to_owned());
        assert!(!diags.has_errors(), "{:?}", diags.errors());
    }

    #[test]
    fn check_file_reports_val_reassignment() {
        let mut db = TycDatabase::new();
        let diags = check_file(
            &mut db,
            "<test>".to_owned(),
            "let x: int = 1\nx = 2\n".to_owned(),
        );
        assert!(diags.has_errors());
    }

    // ── ? operator context enforcement ──────────────────────────────────────

    #[test]
    fn check_file_question_op_valid_in_result_fn() {
        let src = "\
def parse(s: str) -> Result[int, str]:
    let n = int(s)?
    return Ok(n)
";
        let mut db = TycDatabase::new();
        let diags = check_file(&mut db, "<test>".into(), src.to_owned());
        assert!(
            !diags
                .errors()
                .iter()
                .any(|e| format!("{e}").contains("module level")
                    || format!("{e}").contains("returning `")),
            "valid ? usage should not produce context errors: {:?}",
            diags.errors()
        );
    }

    #[test]
    fn check_file_question_op_at_module_level_is_error() {
        let src = "let x = load()?\n";
        let mut db = TycDatabase::new();
        let diags = check_file(&mut db, "<test>".into(), src.to_owned());
        assert!(diags.has_errors());
        let has_qop_error = diags
            .errors()
            .iter()
            .any(|e| format!("{e}").contains("module level"));
        assert!(
            has_qop_error,
            "expected module-level ? error, got: {:?}",
            diags.errors()
        );
    }

    #[test]
    fn check_file_question_op_in_none_fn_is_error() {
        let src = "def run() -> None:\n    let x = fetch()?\n";
        let mut db = TycDatabase::new();
        let diags = check_file(&mut db, "<test>".into(), src.to_owned());
        assert!(diags.has_errors());
        let has_qop_error = diags
            .errors()
            .iter()
            .any(|e| format!("{e}").contains("None"));
        assert!(
            has_qop_error,
            "expected return-type ? error, got: {:?}",
            diags.errors()
        );
    }

    // ── unused import warnings ───────────────────────────────────────────────

    #[test]
    fn check_file_unused_import_produces_warning() {
        let src = "import os\nlet x: int = 1\n";
        let mut db = TycDatabase::new();
        let diags = check_file(&mut db, "<test>".into(), src.to_owned());
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert!(
            diags.warning_count() > 0,
            "expected unused-import warning for `os`"
        );
    }

    #[test]
    fn check_file_unused_lazy_import_anchors_on_original_source() {
        // FINDINGS #15: when an unused-import diagnostic fires on a
        // `lazy import` line, it must render the user-written
        // `lazy import np = math` line rather than the preprocessor's
        // synthesised `import math as np`. The label byte-offset is the
        // alias's position in the original source.
        let src = "lazy import np = math\n";
        let mut db = TycDatabase::new();
        let diags = check_file(&mut db, "<test>".into(), src.to_owned());
        // The warning is promoted to an error by default strictness;
        // either way it must be present.
        let all: Vec<&TycError> = diags.errors().iter().chain(diags.warnings()).collect();
        let unused: &TycError = all
            .iter()
            .copied()
            .find(|e| matches!(e, TycError::UnusedImport { .. }))
            .expect("expected an unused_import diagnostic");
        // Verify the rewritten source + span flow through.
        if let TycError::UnusedImport { src, span, .. } = unused {
            let source_text: &str = src.inner();
            assert!(
                source_text.contains("lazy import np = math"),
                "diagnostic must quote the user-written line; got:\n{source_text}"
            );
            assert!(
                !source_text.contains("import math as np"),
                "preprocessor rewrite must not leak into the diagnostic; got:\n{source_text}"
            );
            // The span anchor must land on the alias `np`, not on
            // `math` (where the preprocessed `import math as np` would
            // have put it).
            let offset: usize = span.offset();
            assert_eq!(
                &source_text[offset..offset + 2],
                "np",
                "span must point at the alias `np`; got `{}` at offset {offset} in:\n{source_text}",
                &source_text[offset..offset + 2.min(source_text.len() - offset)]
            );
        } else {
            unreachable!("matched above");
        }
    }

    #[test]
    fn check_file_unused_lazy_import_anchors_correctly_with_line_drift() {
        // Regression for the Codex P2 review on PR #51: a line-drifting
        // sugar pass (here, a multi-line `guard`) inserts lines above
        // a `lazy import`, so the preprocessor's `line_index` in the
        // expanded source no longer maps directly into the original
        // source. The remap builder must scan the original source
        // independently and pair by position.
        //
        // Original layout: lazy import is at line 8.
        // After multi-line guard expansion (1 header → 3 lines, +2):
        // lazy import shifts to expanded line 10.
        // Without the fix, the remap would point at original line 10
        // (`return 0`), where there's no `lazy import` prefix — so
        // the remap would silently drop and the user would see the
        // preprocessor-rewritten `import math as np` again.
        let src = "\
def f(x: int?) -> int:
    guard v = x else:
        print(\"oops\")
        return 0
    return v

lazy import np = math
";
        let mut db = TycDatabase::new();
        let diags = check_file(&mut db, "<test>".into(), src.to_owned());
        let all: Vec<&TycError> = diags.errors().iter().chain(diags.warnings()).collect();
        let unused = all
            .iter()
            .copied()
            .find(|e| matches!(e, TycError::UnusedImport { .. }))
            .expect("expected an unused_import diagnostic");
        if let TycError::UnusedImport { src, span, .. } = unused {
            let source_text: &str = src.inner();
            assert!(
                source_text.contains("lazy import np = math"),
                "remap must survive the line drift; got:\n{source_text}"
            );
            let offset: usize = span.offset();
            assert_eq!(
                &source_text[offset..offset + 2],
                "np",
                "span must still point at `np` after line drift; got `{}` at offset {offset}",
                &source_text[offset..offset + 2.min(source_text.len() - offset)]
            );
        } else {
            unreachable!("matched above");
        }
    }

    #[test]
    fn check_file_used_import_no_warning() {
        let src = "import os\nlet sep: str = os.sep\n";
        let mut db = TycDatabase::new();
        let diags = check_file(&mut db, "<test>".into(), src.to_owned());
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        assert_eq!(diags.warning_count(), 0, "used import must not warn");
    }

    // ── integration: class and model programs ────────────────────────────────

    #[test]
    fn check_file_plain_class_type_checks() {
        let src = "\
class Point:
    x: int
    y: int

let p: Point = Point(x=1, y=2)
";
        let mut db = TycDatabase::new();
        let diags = check_file(&mut db, "<test>".into(), src.to_owned());
        assert!(!diags.has_errors(), "{:?}", diags.errors());
    }

    #[test]
    fn check_file_model_class_type_checks() {
        let src = "\
model User:
    id: int
    name: str

let u: User = User(id=1, name=\"Ada\")
";
        let mut db = TycDatabase::new();
        let diags = check_file(&mut db, "<test>".into(), src.to_owned());
        assert!(!diags.has_errors(), "{:?}", diags.errors());
    }

    // ── cross-module shape propagation ──────────────────────────────────
    //
    // `check_file_with_imports` walks each module's resolver bindings,
    // looks every import up in the project shape registry, and seeds
    // the imported class's `InterfaceShape` under the local alias. The
    // result: constructor / method arity checks fire on imported
    // symbols, not just locally-declared ones.

    fn build_registry(
        pairs: &[(&str, &str)],
    ) -> std::sync::Arc<std::collections::HashMap<String, ModuleShapes>> {
        let mut shapes = std::collections::HashMap::new();
        for (dotted, text) in pairs {
            shapes.insert(
                (*dotted).to_owned(),
                extract_shapes_for_path("<test>", text),
            );
        }
        std::sync::Arc::new(shapes)
    }

    #[test]
    fn cross_module_ctor_missing_required_field_errors() {
        let lib = "\
class ApiClient:
    api_key: str
    base_url: str
";
        let registry = build_registry(&[("clients", lib)]);
        let main = "\
from clients import ApiClient

let c: ApiClient = ApiClient(base_url=\"https://api.example.com\")
";
        let mut db = TycDatabase::new();
        let diags = check_file_with_imports(&mut db, "main.ty".into(), main.into(), &registry);
        assert!(diags.has_errors(), "cross-module ctor must error");
        let msg = format!("{}", diags.errors()[0]);
        // 0.2.3 swapped the count-based message for the named-missing
        // diagnostic when we can identify which field wasn't filled.
        assert!(
            msg.contains("ApiClient")
                && msg.contains("missing required argument")
                && msg.contains("api_key"),
            "got: {msg}"
        );
    }

    #[test]
    fn cross_module_ctor_all_fields_filled_passes() {
        let lib = "\
class ApiClient:
    api_key: str
    base_url: str
";
        let registry = build_registry(&[("clients", lib)]);
        let main = "\
from clients import ApiClient

let c: ApiClient = ApiClient(api_key=\"k\", base_url=\"u\")
";
        let mut db = TycDatabase::new();
        let diags = check_file_with_imports(&mut db, "main.ty".into(), main.into(), &registry);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
    }

    #[test]
    fn cross_module_method_missing_arg_errors() {
        let lib = "\
class ApiClient:
    api_key: str
    base_url: str

impl ApiClient:
    def url(self, path: str) -> str:
        return self.base_url + path
";
        let registry = build_registry(&[("clients", lib)]);
        let main = "\
from clients import ApiClient

def f() -> None:
    let c: ApiClient = ApiClient(api_key=\"k\", base_url=\"u\")
    let s: str = c.url()
";
        let mut db = TycDatabase::new();
        let diags = check_file_with_imports(&mut db, "main.ty".into(), main.into(), &registry);
        assert!(diags.has_errors(), "cross-module method arity must error");
        let msg = format!("{}", diags.errors()[0]);
        // 0.2.3: when we can name the missing parameter (here `path`),
        // the dedicated `missing_argument` diagnostic fires instead of
        // the count-based `arg_count` form.
        assert!(
            msg.contains("url")
                && msg.contains("missing required argument")
                && msg.contains("path"),
            "got: {msg}"
        );
    }

    #[test]
    fn cross_module_method_correct_arity_passes() {
        let lib = "\
class ApiClient:
    api_key: str
    base_url: str

impl ApiClient:
    def url(self, path: str) -> str:
        return self.base_url + path
";
        let registry = build_registry(&[("clients", lib)]);
        let main = "\
from clients import ApiClient

def f() -> None:
    let c: ApiClient = ApiClient(api_key=\"k\", base_url=\"u\")
    let s: str = c.url(\"/v1\")
";
        let mut db = TycDatabase::new();
        let diags = check_file_with_imports(&mut db, "main.ty".into(), main.into(), &registry);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
    }

    #[test]
    fn cross_module_unknown_module_falls_back_gracefully() {
        // No registry entry for the imported module — the cross-module
        // check is a no-op, matching the per-file semantics
        // (`tyc::implicit_any` and friends would still flag misuse if
        // the user tried to consume the imported value).
        let registry: std::sync::Arc<std::collections::HashMap<String, ModuleShapes>> =
            std::sync::Arc::new(std::collections::HashMap::new());
        let main = "\
from nonexistent import Thing

unsafe:
    let t = Thing()
";
        let mut db = TycDatabase::new();
        let diags = check_file_with_imports(&mut db, "main.ty".into(), main.into(), &registry);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
    }

    #[test]
    fn bare_import_dotted_ctor_missing_required_errors() {
        // `import M as N; N.Cls(...)` — the dotted constructor call
        // now arity-checks against `M`'s shape registry entry. The
        // local `N` binds to `Type::Module("M")`; attribute access
        // resolves `N.Cls` to `Type::Class("Cls")` with the foreign
        // shape installed lazily into the checker's class table.
        let lib = "\
class ApiClient:
    api_key: str
    base_url: str
";
        let registry = build_registry(&[("clients", lib)]);
        let main = "\
import clients

def f() -> None:
    let c = clients.ApiClient(base_url=\"x\")
";
        let mut db = TycDatabase::new();
        let diags = check_file_with_imports(&mut db, "main.ty".into(), main.into(), &registry);
        assert!(diags.has_errors(), "bare-import dotted ctor must error");
        let msg = format!("{}", diags.errors()[0]);
        // 0.2.3: named-missing diagnostic. The class name is
        // module-qualified (`clients.ApiClient`) so multiple imports
        // exposing `ApiClient` remain disambiguated.
        assert!(
            msg.contains("ApiClient")
                && msg.contains("missing required argument")
                && msg.contains("api_key"),
            "got: {msg}"
        );
    }

    #[test]
    fn bare_import_aliased_dotted_method_missing_arg_errors() {
        let lib = "\
class ApiClient:
    api_key: str
    base_url: str

impl ApiClient:
    def url(self, path: str) -> str:
        return self.base_url + path
";
        let registry = build_registry(&[("clients", lib)]);
        let main = "\
import clients as c_mod

def f() -> None:
    let c = c_mod.ApiClient(api_key=\"k\", base_url=\"u\")
    let s: str = c.url()
";
        let mut db = TycDatabase::new();
        let diags = check_file_with_imports(&mut db, "main.ty".into(), main.into(), &registry);
        assert!(diags.has_errors(), "aliased dotted method arity must error");
    }

    #[test]
    fn bare_import_dotted_ctor_correct_passes() {
        let lib = "\
class ApiClient:
    api_key: str
    base_url: str
";
        let registry = build_registry(&[("clients", lib)]);
        let main = "\
import clients

def f() -> None:
    let c = clients.ApiClient(api_key=\"k\", base_url=\"u\")
";
        let mut db = TycDatabase::new();
        let diags = check_file_with_imports(&mut db, "main.ty".into(), main.into(), &registry);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
    }

    #[test]
    fn cross_module_imported_function_arity_checked() {
        let lib = "\
def add(a: int, b: int) -> int:
    return a + b
";
        let registry = build_registry(&[("mathlib", lib)]);
        let main = "\
from mathlib import add

let r: int = add(1)
";
        let mut db = TycDatabase::new();
        let diags = check_file_with_imports(&mut db, "main.ty".into(), main.into(), &registry);
        assert!(diags.has_errors(), "cross-module function arity must error");
    }

    // ── PR-review-driven regression tests ──────────────────────────────

    #[test]
    fn bare_imports_with_same_class_name_dont_collide() {
        // Both `a` and `b` export a class named `Client` with
        // *different* required-field sets. The first-resolved should
        // not "win" for the second module's call site — each call
        // arity-checks against its own shape. FINDINGS — gemini high
        // + codex P1 review of v0.2.0.
        let mod_a = "\
class Client:
    api_key: str
";
        let mod_b = "\
class Client:
    api_key: str
    base_url: str
";
        let registry = build_registry(&[("a", mod_a), ("b", mod_b)]);
        // `a.Client(api_key="k")` is OK (one required field).
        // `b.Client(api_key="k")` is missing `base_url`.
        let main = "\
import a
import b

def f() -> None:
    let ca = a.Client(api_key=\"k\")
    let cb = b.Client(api_key=\"k\")
";
        let mut db = TycDatabase::new();
        let diags = check_file_with_imports(&mut db, "main.ty".into(), main.into(), &registry);
        assert!(diags.has_errors(), "b.Client missing base_url must error");
        let msg = format!("{}", diags.errors()[0]);
        assert!(
            msg.contains("b.Client"),
            "diagnostic should name `b.Client`, not bare `Client`; got: {msg}"
        );
    }

    #[test]
    fn bare_imports_with_same_function_name_dont_collide() {
        // Same as above for free functions: `a.parse(s)` has one
        // arg, `b.parse(s, n)` has two. The lookup must dispatch on
        // the qualified module path. FINDINGS — codex P1 review.
        let mod_a = "\
def parse(s: str) -> int:
    return 1
";
        let mod_b = "\
def parse(s: str, n: int) -> int:
    return n
";
        let registry = build_registry(&[("a", mod_a), ("b", mod_b)]);
        let main = "\
import a
import b

def f() -> None:
    let x: int = a.parse(\"x\")
    let y: int = b.parse(\"y\")
";
        let mut db = TycDatabase::new();
        let diags = check_file_with_imports(&mut db, "main.ty".into(), main.into(), &registry);
        assert!(diags.has_errors(), "b.parse missing second arg must error");
    }

    #[test]
    fn model_required_field_after_default_is_required() {
        // Pydantic-style: `id: int = 1; name: str` — `name` is
        // required even though it follows a defaulted field. The
        // emitted `BaseModel` validates this at runtime; check time
        // should match. FINDINGS — codex P1 review of v0.2.0.
        let main = "\
model User:
    id: int = 1
    name: str

let u: User = User(id=2)
";
        let mut db = TycDatabase::new();
        let registry: std::sync::Arc<std::collections::HashMap<String, ModuleShapes>> =
            std::sync::Arc::new(std::collections::HashMap::new());
        let diags = check_file_with_imports(&mut db, "main.ty".into(), main.into(), &registry);
        assert!(
            diags.has_errors(),
            "required field after default must be flagged"
        );
    }

    #[test]
    fn model_required_after_default_filled_by_kw_passes() {
        let main = "\
model User:
    id: int = 1
    name: str

let u: User = User(name=\"Ada\")
";
        let mut db = TycDatabase::new();
        let registry: std::sync::Arc<std::collections::HashMap<String, ModuleShapes>> =
            std::sync::Arc::new(std::collections::HashMap::new());
        let diags = check_file_with_imports(&mut db, "main.ty".into(), main.into(), &registry);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
    }

    #[test]
    fn impl_field_with_default_treated_as_optional() {
        // `impl X: y: int = 1` should be merged with field_defaults
        // intact so `X()` doesn't (wrongly) error on `y` being
        // missing. FINDINGS — copilot review of v0.2.0.
        let main = "\
class X:
    x: int

impl X:
    y: int = 1

let v: X = X(x=1)
";
        let mut db = TycDatabase::new();
        let registry: std::sync::Arc<std::collections::HashMap<String, ModuleShapes>> =
            std::sync::Arc::new(std::collections::HashMap::new());
        let diags = check_file_with_imports(&mut db, "main.ty".into(), main.into(), &registry);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
    }

    #[test]
    fn check_file_result_ok_err_in_scope() {
        let src = "\
def divide(a: int, b: int) -> Result[int, str]:
    if b == 0:
        return Err(\"division by zero\")
    return Ok(a // b)
";
        let mut db = TycDatabase::new();
        let diags = check_file(&mut db, "<test>".into(), src.to_owned());
        assert!(!diags.has_errors(), "{:?}", diags.errors());
    }

    #[test]
    fn check_file_result_error_mismatch_via_question_op() {
        // FINDINGS #13 polish: when `?` propagates an `Err[E1]` into a
        // function returning `Result[T, E2]` with `E1 != E2`, the diagnostic
        // must carry the dedicated `tyc::result_error_mismatch` code rather
        // than the generic `tyc::type_mismatch`.
        let src = "\
def parse_port(raw: str) -> Result[int, str]:
    return Ok(int(raw))

def bad(raw: str) -> Result[int, int]:
    let n: int = parse_port(raw)?
    return Ok(n)
";
        let mut db = TycDatabase::new();
        let diags = check_file(&mut db, "<test>".into(), src.to_owned());
        assert!(diags.has_errors(), "?-op error mismatch must error");
        assert!(
            diags
                .errors()
                .iter()
                .any(|e| matches!(e, TycError::ResultErrorMismatch { .. })),
            "expected ResultErrorMismatch variant, got: {:?}",
            diags.errors()
        );
    }

    #[test]
    fn check_file_result_matching_errs_no_diagnostic() {
        // Sanity check: matching error types through `?` continue to type-
        // check clean (no false positives from the new detection path).
        let src = "\
def parse_port(raw: str) -> Result[int, str]:
    return Ok(int(raw))

def good(raw: str) -> Result[int, str]:
    let n: int = parse_port(raw)?
    return Ok(n)
";
        let mut db = TycDatabase::new();
        let diags = check_file(&mut db, "<test>".into(), src.to_owned());
        assert!(!diags.has_errors(), "{:?}", diags.errors());
    }

    #[test]
    fn check_file_comptime_binding_recognised() {
        let src = "\
comptime let PORT: int = 8080

def main() -> None:
    print(PORT)
";
        let mut db = TycDatabase::new();
        let diags = check_file(&mut db, "<test>".into(), src.to_owned());
        assert!(!diags.has_errors(), "{:?}", diags.errors());
    }

    #[test]
    fn check_file_nullable_annotation_accepted() {
        // Verify that `T?` nullable sugar in a parameter annotation doesn't
        // cause spurious parse or resolve errors — the preprocessor rewrites
        // `str?` to `str | None` before the Python parser sees it.
        let src = "\
def f(x: str?) -> None:
    print(x)
";
        let mut db = TycDatabase::new();
        let diags = check_file(&mut db, "<test>".into(), src.to_owned());
        assert!(!diags.has_errors(), "{:?}", diags.errors());
    }

    #[test]
    fn resolved_module_query_returns_module_decl() {
        let db = TycDatabase::new();
        let file = SourceFile::new(&db, "test.ty".into(), "let x: int = 1\n".into());
        let resolved = resolved_module(&db, file);
        assert!(
            resolved
                .module_scope()
                .bindings
                .iter()
                .any(|b| b.name == "x"),
            "resolved_module should expose the let binding"
        );
    }

    // ── check_diagnostics Salsa cache ────────────────────────────────────────

    #[test]
    fn check_diagnostics_cached_on_unchanged_source() {
        // Calling `check_diagnostics` twice on the same `SourceFile` with the
        // same text must return the same `Arc` (pointer equality) — i.e. the
        // Salsa cache was hit and the pipeline was not re-executed.
        let db = TycDatabase::new();
        let sf = SourceFile::new(&db, "<test>".to_owned(), "let x: int = 1\n".to_owned());
        let d1 = check_diagnostics(&db, sf);
        let d2 = check_diagnostics(&db, sf);
        assert!(
            std::sync::Arc::ptr_eq(&d1.0, &d2.0),
            "second call must be a Salsa cache hit (same Arc pointer)"
        );
    }

    #[test]
    fn preprocessed_full_shared_across_queries() {
        // The whole point of `preprocessed_full` is that downstream
        // tracked queries (`preprocessed_text`, `resolved_module`,
        // `module_decl_names`) share its result. After a single
        // revision, each subsequent call returns identical data with
        // no extra preprocess pass.
        let db = TycDatabase::new();
        let sf = SourceFile::new(
            &db,
            "<test>".to_owned(),
            "let x: int = 1\ndef f() -> None:\n    pass\n".to_owned(),
        );
        let p1 = preprocessed_full(&db, sf);
        let p2 = preprocessed_full(&db, sf);
        assert!(
            std::sync::Arc::ptr_eq(&p1.0, &p2.0),
            "second call must hit the Salsa cache"
        );
        // Downstream queries should produce consistent results.
        let text = preprocessed_text(&db, sf);
        assert_eq!(text, p1.python_source);
        let names = module_decl_names(&db, sf);
        assert!(names.contains(&"x".to_owned()));
        assert!(names.contains(&"f".to_owned()));
    }

    #[test]
    fn check_source_file_with_imports_uses_cache() {
        // `check_source_file_with_imports` should benefit from the
        // tracked preprocess + resolve queries — calling it twice
        // with the same SourceFile (no `set_text`) should return
        // structurally equivalent diagnostics without re-running the
        // expensive preprocess pass.
        let mut db = TycDatabase::new();
        let sf = SourceFile::new(&db, "<test>".to_owned(), "let x: int = 1\n".to_owned());
        let registry: std::sync::Arc<std::collections::HashMap<String, ModuleShapes>> =
            std::sync::Arc::new(std::collections::HashMap::new());
        let p_before = preprocessed_full(&db, sf);
        let diags = check_source_file_with_imports(&mut db, sf, &registry);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        let p_after = preprocessed_full(&db, sf);
        assert!(
            std::sync::Arc::ptr_eq(&p_before.0, &p_after.0),
            "the imports-aware check must consume the cached \
             preprocess result, not allocate a new one"
        );
    }

    #[test]
    fn check_diagnostics_invalidated_after_set_text() {
        // After `set_text`, Salsa invalidates the cached entry and the next
        // call re-runs the pipeline, returning a new `Arc`.
        use salsa::Setter;
        let mut db = TycDatabase::new();
        let sf = SourceFile::new(&db, "<test>".to_owned(), "let x: int = 1\n".to_owned());
        let d1 = check_diagnostics(&db, sf);
        sf.set_text(&mut db)
            .to("let y: str = \"hello\"\n".to_owned());
        let d2 = check_diagnostics(&db, sf);
        assert!(
            !std::sync::Arc::ptr_eq(&d1.0, &d2.0),
            "Arc must differ after set_text (cache was invalidated)"
        );
        assert!(!d2.has_errors(), "new content should be clean");
    }
}
