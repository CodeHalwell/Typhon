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
        line_byte_starts, validate_extend_usage, validate_lazy_usage, validate_question_ops,
        PreprocessResult,
    },
};
use tyc_types::{check_module_with_options, ExternalShapes, InterfaceShape};

/// Re-export so the CLI and LSP can pass the project's `[python] target`
/// without depending on `tyc-types` directly.
pub use tyc_types::CheckOptions;

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
#[salsa::tracked(returns(clone))]
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
// SAFETY: the wrapper holds only `'static` data behind an `Arc` and
// references no salsa-interned or tracked struct; equality is pointer
// identity (see `PartialEq` above), which is conservative but sound.
unsafe impl salsa::SalsaValue for ArcPreprocessResult {}

/// Tracked query: run sugar-expansion + the preprocessor and cache the
/// full [`PreprocessResult`].
///
/// Both [`preprocessed_text`] and [`resolved_module`] need the same
/// expand-then-preprocess pipeline; sharing it through this query means
/// each source-text change runs the work exactly once instead of three
/// times (preprocessed_text, resolved_module, and the check pipeline).
#[salsa::tracked(returns(clone))]
pub fn preprocessed_full(db: &dyn salsa::Database, file: SourceFile) -> ArcPreprocessResult {
    let text = file.text(db);
    // The mapped chain leaves `line_map` (preprocessed line → `.ty` line) on
    // the result, which `check_pipeline` uses to report the line the user
    // wrote rather than the preprocessed buffer's.
    ArcPreprocessResult(shared_preprocess(text))
}

/// `expand_and_preprocess_mapped(text, false)`, shared between
/// [`preprocessed_full`] and the shape pre-pass ([`parse_for_shapes`]):
/// `tyc build` and `tyc check` extract every file's shapes before checking
/// it, and both used to run the whole sugar pipeline on the same text. Keyed
/// by the full source text, so a hit is exact; bounded, so a long-running
/// language server does not keep every edit it has seen.
fn shared_preprocess(text: &str) -> Arc<PreprocessResult> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    type Cache = Mutex<HashMap<String, Arc<PreprocessResult>>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    const CAPACITY: usize = 1024;
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(text).cloned()) {
        return hit;
    }
    let result = Arc::new(tyc_syntax::preprocess::expand_and_preprocess_mapped(
        text, false,
    ));
    if let Ok(mut c) = cache.lock() {
        if c.len() >= CAPACITY {
            c.clear();
        }
        c.insert(text.to_owned(), Arc::clone(&result));
    }
    result
}

/// Tracked query: the names declared at the top level of the module.
///
/// This is a cheap proxy for "module resolution": it parses the
/// preprocessed source and returns the list of top-level binding names.
/// The full [`ResolvedModule`](tyc_resolve::ResolvedModule) isn't yet
/// `salsa::Update`-friendly, so this is the slice of the resolve step
/// that's salsa-cacheable today.
#[salsa::tracked(returns(clone))]
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
// SAFETY: the wrapper holds only `'static` data behind an `Arc` and
// references no salsa-interned or tracked struct; equality is pointer
// identity (see `PartialEq` above), which is conservative but sound.
unsafe impl salsa::SalsaValue for ArcResolvedModule {}

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
// SAFETY: the wrapper holds only `'static` data behind an `Arc` and
// references no salsa-interned or tracked struct; equality is pointer
// identity (see `PartialEq` above), which is conservative but sound.
unsafe impl salsa::SalsaValue for ArcDiagnostics {}

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
///
/// The body is [`check_pipeline`] with no cross-module registry — the exact
/// same function [`check_source_file_with_imports`] runs, so the single-file
/// path can never drift from the project path again (F55).
#[salsa::tracked(returns(clone))]
fn check_diagnostics(db: &dyn salsa::Database, file: SourceFile) -> ArcDiagnostics {
    ArcDiagnostics::new(check_pipeline(db, file, None, CheckOptions::default()))
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
#[salsa::tracked(returns(clone))]
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
    resolved_module(db, file).resolved_arc()
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

/// Push new source text into a [`SourceFile`] input, but **only** when it
/// actually differs from what the database already holds. Returns `true` when
/// the write happened (i.e. this was a genuine edit), `false` when the call
/// was a no-op.
///
/// This is the one sanctioned way to update a `SourceFile`'s text, and it
/// exists because Salsa's own `Setter::to` is *not* value-comparing: it stamps
/// the field as written at the current revision before it even looks at the
/// new value, so re-uploading identical text invalidates every downstream memo
/// (F56). The LSP re-uploads every project file's text on every keystroke;
/// without this guard `module_shapes_query` re-parsed the whole project per
/// character typed, which is precisely the incrementality `tyc-db` exists to
/// provide.
///
/// Keeping the comparison here (rather than at each call site) also honours the
/// project's wrap-external-crates rule: `salsa::Setter` is named in exactly one
/// place, so a change to its semantics has a one-function blast radius.
pub fn set_source_text(db: &mut TycDatabase, file: SourceFile, text: String) -> bool {
    use salsa::Setter;
    // Scope the immutable borrow so the `&mut` reborrow below is legal.
    let unchanged = {
        let current: &String = file.text(&*db);
        *current == text
    };
    if unchanged {
        return false;
    }
    file.set_text(db).to(text);
    true
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
/// Runs the same preprocess + parse front-end as [`check_pipeline`], but
/// stops there. Returns an empty [`ModuleShapes`] on any parse error
/// — the real diagnostic surfaces when the file is checked for real.
pub fn extract_shapes_for_path(path: &str, text: &str) -> ModuleShapes {
    match parse_for_shapes(text) {
        Some((prep, module)) => source_provenance(path, shapes_of(&prep, &module)),
        None => ModuleShapes::default(),
    }
}

/// Only a `.ty` source's plain `def` is known synchronous: a `.dty` stub or
/// a bundled stub (`path` is then a module name) describes code the checker
/// cannot see, so its functions keep `declared_sync = false`.
fn source_provenance(path: &str, mut shapes: ModuleShapes) -> ModuleShapes {
    if !path.ends_with(".ty") {
        for info in shapes.function_arities.values_mut() {
            info.declared_sync = false;
        }
    }
    shapes
}

/// [`extract_shapes_for_path`] plus the module's
/// [`tyc_analyse::TypeFacts`] — the declared field and return types the
/// `extend BUILTIN:` call-site rewrite consults for imported names —
/// from the same preprocess + parse. `tyc build` uses this for its
/// project-wide pre-pass so the rewrite sees `post.title` on an imported
/// `Post`, or `make()` on an imported function, as the built-in it was
/// declared to be. Both halves are empty on a parse error.
pub fn extract_shapes_and_facts_for_path(
    path: &str,
    text: &str,
) -> (ModuleShapes, tyc_analyse::TypeFacts) {
    match parse_for_shapes(text) {
        Some((prep, module)) => {
            let facts = tyc_analyse::collect_module_type_facts(&module);
            (source_provenance(path, shapes_of(&prep, &module)), facts)
        }
        None => (ModuleShapes::default(), tyc_analyse::TypeFacts::default()),
    }
}

/// The preprocess + parse front-end shared by the shape extractors.
fn parse_for_shapes(text: &str) -> Option<(Arc<PreprocessResult>, tyc_syntax::ast::ModModule)> {
    // The same canonical chain (and so the same result) as the check
    // pipeline's `preprocessed_full`, which reuses it.
    let prep = shared_preprocess(text);
    let module = parse_module(&prep.python_source).ok()?.into_syntax();
    Some((prep, module))
}

fn shapes_of(prep: &PreprocessResult, module: &tyc_syntax::ast::ModModule) -> ModuleShapes {
    // Frozen-ness is preprocessor line-based (the `frozen` modifier is
    // stripped before parsing), so it isn't visible to the AST alone.
    // Compute it here where the preprocess metadata is in scope and hand
    // it to the extractor: a consumer of an imported frozen class sees it
    // as frozen, and variance inference treats its fields as read-only.
    let frozen =
        tyc_types::frozen_class_names(&prep.python_source, &module.body, &prep.frozen_class_lines);
    let mut shapes = tyc_types::extract_module_shapes_with(module, &frozen);
    shapes.frozen_classes = frozen;
    shapes
}

/// Curated, compiler-bundled `.dty` stubs for popular third-party libraries
/// whose own packaging defeats venv introspection (httpx and friends are the
/// motivating case — typed, yet `inspect.signature` over the installed
/// package can't recover a usable shape). Each entry is `(dotted_module,
/// embedded `.dty` source)`. The "long tail" of dependencies stays best-effort
/// (venv introspection / `ty` / an authored `.dty`); this is just the head.
const BUNDLED_STUBS: &[(&str, &str)] = &[
    ("httpx", include_str!("bundled/httpx.dty")),
    ("requests", include_str!("bundled/requests.dty")),
];

/// Seed `shapes` with the bundled stub for any module not already present, so
/// the most-imported third-party libraries get real compile-time checking out
/// of the box. Gap-fill only: an authored project `.dty`/`.ty` already in
/// `shapes` wins. Call this **before** venv enrichment — the venv pass skips
/// modules already in `shapes`, so a bundled stub both supplies the shape and
/// suppresses the `unintrospectable-dependency` warning for that module.
///
/// Every class shape from a bundled stub is marked `partial`: the stub is
/// curated but not guaranteed complete, so members it omits stay lenient (no
/// false `attribute_not_found`), while the members it models still contribute
/// real types (return types, key fields). Request methods carry
/// `**kwargs: object` so a call passing httpx's many optional kwargs isn't
/// rejected, and client constructors enumerate the common kwargs as optional
/// fields.
pub fn seed_bundled_stubs(shapes: &mut std::collections::HashMap<String, ModuleShapes>) {
    for (module, source) in BUNDLED_STUBS {
        if shapes.contains_key(*module) {
            continue;
        }
        let mut extracted = extract_shapes_for_path(module, source);
        for shape in extracted.class_shapes.values_mut() {
            shape.partial = true;
        }
        shapes.insert((*module).to_owned(), extracted);
    }
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
// SAFETY: the wrapper holds only `'static` data behind an `Arc` and
// references no salsa-interned or tracked struct; equality is pointer
// identity (see `PartialEq` above), which is conservative but sound.
unsafe impl salsa::SalsaValue for ArcModuleShapes {}

/// Salsa-tracked variant of [`extract_shapes_for_path`]. The LSP
/// backend keeps a `HashMap<dotted_name, SourceFile>` per project
/// root and queries this for each file — Salsa re-runs the
/// extraction only on the file whose text changed, so a keystroke
/// in `src/main.ty` doesn't re-parse `src/clients.ty`.
///
/// That only holds if callers update inputs through
/// [`set_source_text`]. Salsa's raw `set_text` is *not* value-comparing
/// (see [`set_source_text`]'s docs), so re-uploading identical text with
/// it invalidates this query for every file in the project.
///
/// The result is wrapped in [`ArcModuleShapes`]; callers typically
/// unwrap via `.0.clone()` to drop the wrapper.
#[salsa::tracked(returns(clone))]
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
    check_file_with_imports_opts(db, path, text, shapes_by_module, CheckOptions::default())
}

/// [`check_file_with_imports`] with explicit [`CheckOptions`] (the project's
/// `[python] target`). `tyc check` and `tyc build` use this.
pub fn check_file_with_imports_opts(
    db: &mut TycDatabase,
    path: String,
    text: String,
    shapes_by_module: &std::sync::Arc<std::collections::HashMap<String, ModuleShapes>>,
    options: CheckOptions,
) -> Diagnostics {
    let file = SourceFile::new(db, path, text);
    check_source_file_with_imports_opts(db, file, shapes_by_module, options)
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
    check_source_file_with_imports_opts(db, file, shapes_by_module, CheckOptions::default())
}

/// [`check_source_file_with_imports`] with explicit [`CheckOptions`] (the
/// project's `[python] target`). The language server uses this.
pub fn check_source_file_with_imports_opts(
    db: &mut TycDatabase,
    file: SourceFile,
    shapes_by_module: &std::sync::Arc<std::collections::HashMap<String, ModuleShapes>>,
    options: CheckOptions,
) -> Diagnostics {
    check_pipeline(&*db, file, Some(shapes_by_module), options)
}

/// The one true check pipeline, shared by the Salsa-tracked
/// [`check_diagnostics`] query (single-file / REPL / LSP fallback path,
/// `shapes_by_module = None`) and [`check_source_file_with_imports`] (the
/// project path, `shapes_by_module = Some(_)`).
///
/// Having exactly one body is deliberate: the two entry points used to be
/// separate implementations and silently drifted — the tracked path skipped
/// the B34 comptime substitution below, so `tyc repl` and the LSP's
/// single-file mode rejected `comptime let T: type = int` programs that
/// `tyc check` accepted (F55). The only thing the registry parameter now
/// changes is whether cross-module [`ExternalShapes`] are seeded.
fn check_pipeline(
    db: &dyn salsa::Database,
    file: SourceFile,
    shapes_by_module: Option<&std::sync::Arc<std::collections::HashMap<String, ModuleShapes>>>,
    options: CheckOptions,
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
    let python_minor = options.python_minor;
    for err in validate_lazy_usage(&text, python_minor) {
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
                path.clone(),
                prep.python_source.clone(),
                e.to_string(),
                usize::from(e.location.start()),
            ));
            diags.remap_lines(&prep.python_source, &prep.line_map, &path, &text);
            return diags;
        }
    };

    // B34: inline comptime values into the AST so the type-checker
    // sees `comptime let T: type = int` as a `type T = int` alias
    // declaration. Without this, `T` resolves as a distinct nominal
    // class and `def f(x: T)` rejects `int` arguments. Matches the
    // same substitution `tyc build` and `tyc run` apply.
    let (comptime_values, _comptime_diags) = tyc_analyse::evaluate_comptime_in_source(
        &module,
        &path,
        &prep.python_source,
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

    // `except*` control-flow validation. This is an *error*, not a lint: a
    // `return` / `break` / `continue` in an `except*` handler makes CPython
    // refuse to compile the emitted file, so it has to run on the pipeline
    // every surface shares — `tyc build` reaches this function, but not the
    // `editor_lint_diagnostics` hook where the rest of the pure-AST checks
    // live. Without it here, `tyc check` reported the error and `tyc build`
    // cheerfully wrote a `build/main.py` that could not be imported.
    diags.extend(tyc_analyse::analyse_except_star_control_flow(
        &module,
        &path,
        &prep.python_source,
    ));

    // `None` here is exactly what the pre-F55 single-file path did: with no
    // registry there is nothing to seed, so the checker runs its
    // in-module-only pass (`check_module_with`).
    let external = shapes_by_module
        .map(|s| build_external_shapes(&resolved_arc, s, std::path::Path::new(&path)));
    let type_diags = check_module_with_options(
        path.clone(),
        &prep.python_source,
        &resolved_arc,
        &module,
        &prep.unsafe_lines,
        &prep.frozen_class_lines,
        &prep.impl_distributed_lines,
        external.as_ref(),
        options,
    )
    .diagnostics;
    diags.extend(type_diags);

    // Every diagnostic above is anchored to the preprocessed buffer; report
    // it against the `.ty` text and line the user actually wrote.
    diags.remap_lines(&prep.python_source, &prep.line_map, &path, &text);

    diags
}

/// The registry key of the module at `path`: the longest key whose dotted
/// spelling matches the path's tail (`shapes.kinds` ↔ `…/shapes/kinds.ty`,
/// `shapes` ↔ `…/shapes/__init__.ty`). `None` when the file is not in the
/// registry (a standalone check).
fn own_module_key<'a>(
    path: &std::path::Path,
    keys: impl Iterator<Item = &'a String>,
) -> Option<String> {
    let p = path.to_string_lossy().replace('\\', "/");
    let mut best: Option<String> = None;
    for key in keys {
        if key.is_empty() {
            continue;
        }
        let rel = key.replace('.', "/");
        let tails = [
            format!("/{rel}.ty"),
            format!("/{rel}.dty"),
            format!("/{rel}/__init__.ty"),
            format!("/{rel}/__init__.dty"),
        ];
        let matches = tails.iter().any(|t| p.ends_with(t.as_str()) || p == t[1..]);
        if matches && best.as_ref().is_none_or(|b| key.len() > b.len()) {
            best = Some(key.clone());
        }
    }
    best
}

/// The absolute registry key an import binding sources from. A relative
/// import walks up from the importing module's package — one level for the
/// first dot, one more for each further dot — and appends the written
/// module, if any; an absolute import is returned as written. Without a
/// known own key the relative name is returned unchanged (and will simply
/// not match the registry, as before).
fn canonical_import_module(
    info: &tyc_resolve::ImportInfo,
    own_key: Option<&str>,
    own_is_init: bool,
) -> String {
    if info.level == 0 {
        return info.module.clone();
    }
    let Some(own) = own_key else {
        return info.module.clone();
    };
    let mut parts: Vec<&str> = own.split('.').filter(|s| !s.is_empty()).collect();
    if !own_is_init {
        parts.pop();
    }
    for _ in 1..info.level {
        parts.pop();
    }
    let mut out: Vec<String> = parts.iter().map(|s| (*s).to_owned()).collect();
    if !info.module.is_empty() {
        out.extend(info.module.split('.').map(str::to_owned));
    }
    out.join(".")
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
    path: &std::path::Path,
) -> ExternalShapes {
    // A relative import (`from .kinds import Shape`) names its source
    // relative to the importing file's package, while the registry is
    // keyed by absolute dotted names (`shapes.kinds`). Resolve the file's
    // own key from the registry and canonicalise every relative import
    // against it (review 2026-09-30 §4.1); absolute imports are unchanged.
    let own_key = own_module_key(path, shapes_by_module.keys());
    let is_init = path.file_stem().is_some_and(|s| s == "__init__");
    let canon = |info: &tyc_resolve::ImportInfo| -> String {
        canonical_import_module(info, own_key.as_deref(), is_init)
    };
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
                    .entry(canon(info).clone())
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
            // uses `canon(info)` (the original dotted name), not the
            // local alias.
            external
                .bare_imports
                .insert(b.name.clone(), canon(info).clone());
            continue;
        };
        let Some(module_shapes) = shapes_by_module.get(&canon(info)) else {
            continue;
        };
        if let Some(shape) = module_shapes.class_shapes.get(member) {
            external.class_shapes.insert(b.name.clone(), shape.clone());
            if let Some(tps) = module_shapes.class_type_params.get(member) {
                external
                    .class_type_params
                    .insert(b.name.clone(), tps.clone());
            }
            // C2 cross-module: re-key the imported generic's inferred
            // per-parameter variance under the local import name so the
            // consumer's `user_generic_param_variance` widens a covariant /
            // contravariant imported generic the same way it does an
            // in-module one. A pure relaxation — absence keeps the
            // invariant default.
            if let Some(variances) = module_shapes.class_param_variance.get(member) {
                external
                    .class_param_variance
                    .insert(b.name.clone(), variances.clone());
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
            // Enum classes and `frozen` classes ARE `class_shapes`
            // entries, so they're handled here in the class branch (the
            // `else if` chain below never reaches them). Re-key both
            // under the local import name.
            if let Some(members) = module_shapes.enums.get(member) {
                external.enums.insert(b.name.clone(), members.clone());
            }
            if module_shapes.frozen_classes.contains(member) {
                external.frozen_classes.insert(b.name.clone());
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
            let remap = local_by_module.get(&canon(info));
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
        } else if let Some(base) = module_shapes.newtypes.get(member) {
            // Newtype alias imported by name (`from foo import ProjectTag`).
            // Re-key its base type under the local import name so the
            // consumer's asymmetric escape-upward rule (`ProjectTag`
            // widens into a `str` slot) fires the same way it would for a
            // locally-declared newtype. The base is published
            // already-resolved (a primitive in the common case), so no
            // per-variant re-keying is needed.
            external.newtypes.insert(b.name.clone(), base.clone());
        } else if let Some(alias) = module_shapes.type_aliases.get(member) {
            // Transparent type alias imported by name (`from foo import
            // Report`). Re-key under the local name; the RHS keeps source
            // class names (resolved nominally by the consumer).
            external.type_aliases.insert(b.name.clone(), alias.clone());
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
        if !modules_touched.insert(canon(info).clone()) {
            continue;
        }
        let Some(module_shapes) = shapes_by_module.get(&canon(info)) else {
            continue;
        };
        let remap = local_by_module.get(&canon(info));
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
        // Same shape of problem for newtypes: an imported class can
        // expose a field / parameter / return typed as a newtype whose
        // NAME the consumer never imported (`from models import
        // AttributedSpend`, where `AttributedSpend.project: ProjectTag`).
        // The published class shape carries the field type under the
        // newtype's *source* name, so seed every newtype the touched
        // module declares under that same source name. Then a
        // `ProjectTag`-typed field flowing into a `str` slot widens via
        // the consumer's escape-upward rule. `entry().or_insert` keeps
        // any alias-keyed entry the per-binding loop already wrote.
        for (newtype_name, base) in &module_shapes.newtypes {
            external
                .newtypes
                .entry(newtype_name.clone())
                .or_insert_with(|| base.clone());
        }
        // Same reasoning for transparent type aliases, enums, and frozen
        // classes reached only through an imported signature (a function
        // returning `Report`, a parameter typed as an imported enum, a
        // field of an imported frozen class). Seed each touched module's
        // declarations under their source names; `entry().or_insert`
        // preserves any alias-keyed entry from the per-binding loop.
        for (alias_name, alias) in &module_shapes.type_aliases {
            external
                .type_aliases
                .entry(alias_name.clone())
                .or_insert_with(|| alias.clone());
        }
        for (enum_name, members) in &module_shapes.enums {
            external
                .enums
                .entry(enum_name.clone())
                .or_insert_with(|| members.clone());
        }
        for frozen_name in &module_shapes.frozen_classes {
            external.frozen_classes.insert(frozen_name.clone());
        }
        // C1 cross-module: an imported class's higher-kinded constructor
        // variables (`F` in `class Functor[F[_]]`) are bare
        // type-parameter identifiers consulted by name when the consumer
        // resolves the imported class's signatures. They carry no class
        // re-keying — they're identity tokens — so seed the touched
        // module's full HKT set. Absence degrades to today's permissive
        // (pre-HKT) handling, so this can only restore a sound check, never
        // introduce a false positive.
        for hkt_name in &module_shapes.hkt_param_names {
            external.hkt_param_names.insert(hkt_name.clone());
        }
        // Variance of a generic reached only through an imported signature
        // (a function returning `Producer[Dog]` where `Producer` itself was
        // not imported by name) — seed under the source name so the
        // consumer can widen it. `entry().or_insert` preserves any
        // alias-keyed entry the per-binding loop already wrote.
        for (cls_name, variances) in &module_shapes.class_param_variance {
            external
                .class_param_variance
                .entry(cls_name.clone())
                .or_insert_with(|| variances.clone());
        }
        // Carry `__typhon_builtin_ext_*` sentinel class shapes from the
        // imported module so the consumer's type checker recognises
        // cross-module builtin extension methods via
        // `is_user_builtin_extension`. Without this, `title.slug()` in
        // a consumer that imported a module declaring `extend str: def
        // slug(...)` would fire `tyc::attribute_not_found`. (#202)
        // When multiple imported modules extend the same built-in, merge
        // their methods into a single shape rather than keeping only the
        // first module's sentinel. (#202 review feedback)
        for (cls_name, shape) in &module_shapes.class_shapes {
            if cls_name.starts_with("__typhon_builtin_ext_") {
                let entry = external
                    .class_shapes
                    .entry(cls_name.clone())
                    .or_insert_with(|| shape.clone());
                // If the entry already existed, merge in any new methods
                // from this module that aren't already present.
                for (method_name, method_sig) in &shape.methods {
                    entry
                        .methods
                        .entry(method_name.clone())
                        .or_insert_with(|| method_sig.clone());
                }
            }
        }
    }
    // Bare module imports (`import models`, then `models.AttributedSpend`)
    // produce bindings with no `member`, so the per-binding loop above
    // skips them entirely — yet `models.AttributedSpend.project: ProjectTag`
    // needs `ProjectTag` in `external.newtypes` to widen into a `str` slot
    // exactly as the `from models import …` form does. Seed each bare-
    // imported module's transparent shapes (newtype / alias / enum / frozen)
    // under their source names. `sealed_unions` stay member-gated — a
    // variant-to-union upcast isn't reachable without the variant in scope —
    // and this runs as a separate pass so it can't perturb the
    // `modules_touched` / variant-remap bookkeeping above. (PR #191 review —
    // chatgpt-codex-connector.)
    for b in bindings {
        let Some(info) = &b.import_info else { continue };
        if info.member.is_some() {
            continue;
        }
        let Some(module_shapes) = shapes_by_module.get(&canon(info)) else {
            continue;
        };
        for (newtype_name, base) in &module_shapes.newtypes {
            external
                .newtypes
                .entry(newtype_name.clone())
                .or_insert_with(|| base.clone());
        }
        for (alias_name, alias) in &module_shapes.type_aliases {
            external
                .type_aliases
                .entry(alias_name.clone())
                .or_insert_with(|| alias.clone());
        }
        for (enum_name, members) in &module_shapes.enums {
            external
                .enums
                .entry(enum_name.clone())
                .or_insert_with(|| members.clone());
        }
        for frozen_name in &module_shapes.frozen_classes {
            external.frozen_classes.insert(frozen_name.clone());
        }
        // Bare-imported module's HKT constructor variables (see the
        // member-import pass above for rationale).
        for hkt_name in &module_shapes.hkt_param_names {
            external.hkt_param_names.insert(hkt_name.clone());
        }
        // C2 cross-module: a bare `import producer` plus a qualified
        // annotation `producer.Producer[Dog]` resolves to
        // `Type::Generic("Producer", …)` — the annotation path drops the
        // module qualifier, so the generic head is the BARE source class
        // name. The consumer's `user_generic_param_variance` looks the
        // per-parameter variance up under that bare head, so seed it here
        // under the source class name (mirroring the member-gated
        // touched-module pass above). Without this the covariant upcast
        // `Producer[Dog] -> Producer[Animal]` falls back to invariant and
        // is wrongly rejected as `tyc::type_mismatch`. A pure relaxation —
        // absence keeps the invariant default, so an actually-invariant
        // imported generic still rejects.
        for (cls_name, variances) in &module_shapes.class_param_variance {
            external
                .class_param_variance
                .entry(cls_name.clone())
                .or_insert_with(|| variances.clone());
        }
    }
    apply_cross_module_extensions(&mut external, bindings, &canon, shapes_by_module);
    external
}

/// The key a `__typhon_extend_<Class>@<spec>` sentinel published by the
/// module keyed `owner` points at: the patched class's module. `owner` may
/// be a package (`pkg` for `pkg/__init__.ty`); the relative spec is
/// resolved as a module first and as a package if that names nothing.
fn extension_target_key(
    owner: &str,
    level: u32,
    module: &str,
    known: &std::collections::HashMap<String, ModuleShapes>,
) -> String {
    let info = tyc_resolve::ImportInfo {
        module: module.to_owned(),
        member: None,
        level,
    };
    let as_module = canonical_import_module(&info, Some(owner), false);
    if level == 0 || known.contains_key(&as_module) {
        return as_module;
    }
    let as_package = canonical_import_module(&info, Some(owner), true);
    if known.contains_key(&as_package) {
        as_package
    } else {
        as_module
    }
}

/// Rewrite every relative `__typhon_extend_` sentinel in `shapes` (the
/// shapes of the module keyed `owner`) to an absolute spec, so the
/// sentinels survive being merged into a `pub *` facade under another key.
pub fn absolutise_extension_sentinels<'a>(
    shapes: &'a ModuleShapes,
    owner: &str,
    owner_is_init: bool,
) -> std::borrow::Cow<'a, ModuleShapes> {
    let relative = shapes.class_shapes.keys().any(|name| {
        tyc_types::parse_extension_sentinel(name).is_some_and(|(_, level, _)| level > 0)
    });
    if !relative {
        return std::borrow::Cow::Borrowed(shapes);
    }
    let mut out = shapes.clone();
    out.class_shapes = shapes
        .class_shapes
        .iter()
        .map(|(name, shape)| {
            let name = match tyc_types::parse_extension_sentinel(name) {
                Some((class, level, module)) if level > 0 => {
                    let info = tyc_resolve::ImportInfo {
                        module: module.to_owned(),
                        member: None,
                        level,
                    };
                    let target = canonical_import_module(&info, Some(owner), owner_is_init);
                    tyc_types::extension_sentinel_name(class, &target)
                }
                _ => name.clone(),
            };
            (name, shape.clone())
        })
        .collect();
    std::borrow::Cow::Owned(out)
}

/// W3-03: `extend User:` in module B patches `User` (declared in A) when B
/// is imported. A consumer that imports B — by name, as a module, or
/// through a `pub *` facade aggregating it — sees the patched methods on
/// its `User`, the local name(s) it bound A's class to, and on A's shape
/// in the registry (a `User` reached through an imported signature or
/// `a.User`). A consumer that does not import B gets no promise that B
/// ran, so it sees no extension. Methods merge first-write-wins: the
/// class's own methods win.
fn apply_cross_module_extensions(
    external: &mut ExternalShapes,
    bindings: &[tyc_resolve::Binding],
    canon: &dyn Fn(&tyc_resolve::ImportInfo) -> String,
    shapes_by_module: &std::sync::Arc<std::collections::HashMap<String, ModuleShapes>>,
) {
    let mut imported: Vec<String> = Vec::new();
    for b in bindings {
        if let Some(info) = &b.import_info {
            let key = canon(info);
            if !imported.contains(&key) {
                imported.push(key);
            }
            // `from pkg import text` binds a submodule.
            if let Some(member) = &info.member {
                let sub = format!("{}.{member}", canon(info));
                if shapes_by_module.contains_key(&sub) && !imported.contains(&sub) {
                    imported.push(sub);
                }
            }
        }
    }
    let mut contributions: Vec<(String, String, InterfaceShape)> = Vec::new();
    for owner in &imported {
        let Some(shapes) = shapes_by_module.get(owner) else {
            continue;
        };
        let mut sentinels: Vec<_> = shapes
            .class_shapes
            .iter()
            .filter_map(|(name, shape)| {
                tyc_types::parse_extension_sentinel(name).map(|parsed| (parsed, shape))
            })
            .collect();
        sentinels.sort_by(|a, b| a.0.cmp(&b.0));
        for ((class, level, module), shape) in sentinels {
            let target = extension_target_key(owner, level, module, shapes_by_module);
            contributions.push((target, class.to_owned(), shape.clone()));
        }
    }
    if contributions.is_empty() {
        return;
    }
    let merge = |into: &mut InterfaceShape, from: &InterfaceShape| {
        for (m, sig) in &from.methods {
            into.methods.entry(m.clone()).or_insert_with(|| sig.clone());
        }
    };
    for (target, class, shape) in &contributions {
        // The consumer's local names for the class: `from a import User
        // [as U]`, or the same class re-exported by a facade it lives under.
        for b in bindings {
            let Some(info) = &b.import_info else { continue };
            if info.member.as_deref() != Some(class.as_str()) {
                continue;
            }
            let source = canon(info);
            if &source != target && !target.starts_with(&format!("{source}.")) {
                continue;
            }
            if let Some(local) = external.class_shapes.get_mut(&b.name) {
                merge(local, shape);
            }
        }
        // The registry entry, for `a.User` and a `User` reached through an
        // imported signature. Copy-on-write: only consumers that import an
        // extending module pay for the clone.
        let registry = std::sync::Arc::make_mut(&mut external.by_module);
        if let Some(cls) = registry
            .get_mut(target)
            .and_then(|m| m.class_shapes.get_mut(class))
        {
            merge(cls, shape);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import(module: &str, level: u32) -> tyc_resolve::ImportInfo {
        tyc_resolve::ImportInfo {
            module: module.to_owned(),
            member: Some("Thing".to_owned()),
            level,
        }
    }

    #[test]
    fn own_module_key_matches_the_longest_dotted_tail() {
        let keys = [
            "pkg".to_owned(),
            "pkg.sub".to_owned(),
            "other.sub".to_owned(),
        ];
        let key = |p: &str| own_module_key(std::path::Path::new(p), keys.iter());
        assert_eq!(key("/proj/src/pkg/sub.ty").as_deref(), Some("pkg.sub"));
        assert_eq!(key("/proj/src/pkg/__init__.ty").as_deref(), Some("pkg"));
        assert_eq!(key("/proj/src/pkg/sub.dty").as_deref(), Some("pkg.sub"));
        assert_eq!(key("/proj/src/unrelated.ty"), None);
    }

    #[test]
    fn relative_imports_resolve_against_the_importing_package() {
        // `from .shapes import Thing` inside `pkg/sub.ty` → `pkg.shapes`.
        assert_eq!(
            canonical_import_module(&import("shapes", 1), Some("pkg.sub"), false),
            "pkg.shapes"
        );
        // …and inside `pkg/__init__.ty` the package itself is the base.
        assert_eq!(
            canonical_import_module(&import("shapes", 1), Some("pkg"), true),
            "pkg.shapes"
        );
        // `from . import Thing` names the package.
        assert_eq!(
            canonical_import_module(&import("", 1), Some("pkg.sub"), false),
            "pkg"
        );
        // `from ..x import Thing` from `pkg/sub.ty` climbs to the root.
        assert_eq!(
            canonical_import_module(&import("x", 2), Some("pkg.sub"), false),
            "x"
        );
        // An absolute import is returned as written.
        assert_eq!(
            canonical_import_module(&import("pkg.shapes", 0), Some("pkg.sub"), false),
            "pkg.shapes"
        );
        // Without a known own key the relative name is left alone.
        assert_eq!(
            canonical_import_module(&import("shapes", 1), None, false),
            "shapes"
        );
    }

    #[test]
    fn seed_bundled_stubs_populates_httpx_and_requests() {
        let mut shapes: std::collections::HashMap<String, ModuleShapes> =
            std::collections::HashMap::new();
        seed_bundled_stubs(&mut shapes);
        // Both flagship HTTP libraries are seeded with their key classes.
        let httpx = shapes.get("httpx").expect("httpx stub seeded");
        assert!(
            httpx.class_shapes.contains_key("AsyncClient"),
            "httpx.AsyncClient"
        );
        let resp = httpx
            .class_shapes
            .get("Response")
            .expect("httpx.Response shape");
        // A modeled member is present and the shape is `partial` (so omitted
        // members stay lenient rather than producing `attribute_not_found`).
        assert!(resp.methods.contains_key("json"), "Response.json modeled");
        assert!(resp.partial, "bundled shapes must be marked partial");
        assert!(shapes.contains_key("requests"), "requests stub seeded");

        // `Response.url` is an `httpx.URL`, not a `str` — the stub typed it
        // as `str`, so `resp.url.host` was rejected and
        // `resp.url.startswith(...)` (which crashes at runtime) was accepted.
        assert_eq!(
            resp.fields.get("url").map(|t| format!("{t:?}")),
            Some("Class(\"httpx.URL\")".to_owned()),
            "Response.url must be typed as httpx.URL"
        );
        assert!(
            httpx.class_shapes.contains_key("URL"),
            "the URL class it refers to must be modeled too"
        );
        // httpx 0.28 removed `proxies=` and `app=`; accepting them let a
        // call that raises `TypeError` at runtime pass `tyc check`.
        for client in ["Client", "AsyncClient"] {
            let shape = httpx
                .class_shapes
                .get(client)
                .unwrap_or_else(|| panic!("httpx.{client} shape"));
            for removed in ["proxies", "app"] {
                assert!(
                    !shape.fields.contains_key(removed),
                    "httpx.{client} must not accept the removed `{removed}=` kwarg"
                );
            }
            assert!(
                shape.fields.contains_key("proxy"),
                "httpx.{client} keeps the replacement `proxy=`"
            );
        }
        // The exception hierarchy `raise_for_status` / a transport failure
        // raises has to exist for `except httpx.HTTPStatusError` to check.
        for exc in [
            "HTTPError",
            "HTTPStatusError",
            "RequestError",
            "TimeoutException",
            "ConnectError",
        ] {
            assert!(
                httpx.class_shapes.contains_key(exc),
                "httpx.{exc} must be modeled"
            );
        }
    }

    #[test]
    fn seed_bundled_stubs_gap_fills_only() {
        // An existing entry (project stub/source) for a bundled module wins.
        let mut shapes: std::collections::HashMap<String, ModuleShapes> =
            std::collections::HashMap::new();
        let sentinel = extract_shapes_for_path("httpx", "class Marker:\n    x: int\n");
        shapes.insert("httpx".to_owned(), sentinel);
        seed_bundled_stubs(&mut shapes);
        let httpx = shapes.get("httpx").expect("httpx present");
        assert!(
            httpx.class_shapes.contains_key("Marker"),
            "project httpx shape must be preserved, not overwritten by the bundle"
        );
        assert!(
            !httpx.class_shapes.contains_key("AsyncClient"),
            "bundle must not override an existing entry"
        );
    }

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

    /// The Dead Reckoning dogfooding report: `models` declares
    /// `newtype ProjectTag = str` and an `AttributedSpend` whose
    /// `project` field is typed `ProjectTag`. A consumer that imports
    /// ONLY `AttributedSpend` (never `ProjectTag` by name) must still
    /// widen `spend.project` into a `str` slot — the newtype is reached
    /// through the imported class's field type, so the fix has to seed
    /// it from `by_module` for every touched module, not just when the
    /// newtype name itself appears in the import list.
    #[test]
    fn cross_module_newtype_field_widens_without_importing_newtype() {
        let models = "\
newtype ProjectTag = str

class AttributedSpend:
    project: ProjectTag
    amount: float
";
        let registry = build_registry(&[("models", models)]);
        let main = "\
from models import AttributedSpend

def first_key(spend: AttributedSpend) -> str:
    let key: str = spend.project
    return key
";
        let mut db = TycDatabase::new();
        let diags = check_file_with_imports(&mut db, "main.ty".into(), main.into(), &registry);
        assert!(
            !diags.has_errors(),
            "imported newtype field must widen to its base type; got: {:?}",
            diags.errors()
        );
    }

    /// Bare `import models` form of the field-widening case: the binding
    /// for `models` carries no `member`, so the per-binding seeding loop
    /// skips it — the dedicated bare-import pass must still seed
    /// `ProjectTag` so `spend.project` widens. (PR #191 review.)
    #[test]
    fn cross_module_newtype_field_widens_via_bare_module_import() {
        let models = "\
newtype ProjectTag = str

class AttributedSpend:
    project: ProjectTag
    amount: float
";
        let registry = build_registry(&[("models", models)]);
        let main = "\
import models

def first_key(spend: models.AttributedSpend) -> str:
    let key: str = spend.project
    return key
";
        let mut db = TycDatabase::new();
        let diags = check_file_with_imports(&mut db, "main.ty".into(), main.into(), &registry);
        assert!(
            !diags.has_errors(),
            "bare-imported newtype field must widen to its base type; got: {:?}",
            diags.errors()
        );
    }

    /// Companion to the above: importing the newtype by name (`from
    /// models import ProjectTag`) and constructing a value with it must
    /// also widen, exercising the per-binding re-keying branch through
    /// the real db path.
    #[test]
    fn cross_module_imported_newtype_name_widens_to_base() {
        let models = "pub newtype ProjectTag = str\n";
        let registry = build_registry(&[("models", models)]);
        let main = "\
from models import ProjectTag

def label(t: ProjectTag) -> str:
    let s: str = t
    return s
";
        let mut db = TycDatabase::new();
        let diags = check_file_with_imports(&mut db, "main.ty".into(), main.into(), &registry);
        assert!(
            !diags.has_errors(),
            "imported newtype name must widen to its base type; got: {:?}",
            diags.errors()
        );
    }

    /// Transparent type alias across modules: `type Report = ReportData`
    /// must `unwrap_alias` in the consumer so a `ReportData` value flows
    /// into a `Report`-typed slot. Reached here both by name (the alias
    /// is imported) and through a return-type-only path.
    #[test]
    fn cross_module_transparent_type_alias_unwraps() {
        let models = "\
class ReportData:
    title: str

type Report = ReportData
";
        let registry = build_registry(&[("models", models)]);
        let main = "\
from models import Report, ReportData

def wrap(d: ReportData) -> Report:
    return d
";
        let mut db = TycDatabase::new();
        let diags = check_file_with_imports(&mut db, "main.ty".into(), main.into(), &registry);
        assert!(
            !diags.has_errors(),
            "imported transparent type alias must unwrap to its target; got: {:?}",
            diags.errors()
        );
    }

    /// Exhaustive `match` over an imported enum must NOT spuriously fire
    /// `missing_return` — the consumer needs the enum's closed member set
    /// (which it never imported by name) to see the match is exhaustive.
    #[test]
    fn cross_module_enum_exhaustive_match_no_false_missing_return() {
        let models = "\
enum Color:
    RED
    GREEN
    BLUE
";
        let registry = build_registry(&[("models", models)]);
        let main = "\
from models import Color

def label(c: Color) -> str:
    match c:
        case Color.RED:
            return \"r\"
        case Color.GREEN:
            return \"g\"
        case Color.BLUE:
            return \"b\"
";
        let mut db = TycDatabase::new();
        let diags = check_file_with_imports(&mut db, "main.ty".into(), main.into(), &registry);
        assert!(
            !diags.has_errors(),
            "exhaustive match over an imported enum must not fire missing_return; got: {:?}",
            diags.errors()
        );
    }

    /// Soundness: a field write on an *imported* `frozen` class must trip
    /// `frozen_assign` just as a local one does — otherwise the consumer
    /// silently accepts a mutation that raises `FrozenInstanceError` at
    /// runtime. (Frozen-ness is preprocessor line-based, so this also
    /// exercises the `frozen_class_names` plumbing in
    /// `extract_shapes_for_path`.)
    #[test]
    fn cross_module_frozen_class_field_write_errors() {
        let models = "\
class Config frozen:
    port: int
";
        let registry = build_registry(&[("models", models)]);
        let main = "\
from models import Config

def mutate(c: Config) -> None:
    c.port = 9999
";
        let mut db = TycDatabase::new();
        let diags = check_file_with_imports(&mut db, "main.ty".into(), main.into(), &registry);
        assert!(
            diags
                .errors()
                .iter()
                .any(|e| e.to_string().contains("frozen")),
            "field write on an imported frozen class must fire frozen_assign; got: {:?}",
            diags
                .errors()
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
        );
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

    /// F55 — the tracked single-file path (`check_file` / `check_source_file`,
    /// used by `tyc repl` and the LSP outside a workspace) used to skip the
    /// B34 comptime substitution that `check_file_with_imports` (and therefore
    /// `tyc check`) applies, so a `comptime let T: type = int` alias resolved
    /// as a distinct nominal class and every call rejected its `int` argument.
    #[test]
    fn tracked_check_applies_comptime_type_substitution() {
        const SRC: &str = "comptime let T: type = int\n\
                           \n\
                           def identity(x: T) -> T:\n    \
                               return x\n\
                           \n\
                           def main() -> None:\n    \
                               let v: int = identity(3)\n    \
                               print(v)\n";
        let mut db = TycDatabase::new();
        let diags = check_file(&mut db, "<comptime>".to_owned(), SRC.to_owned());
        assert!(
            !diags.has_errors(),
            "single-file check must accept a `comptime let T: type` alias: {:?}",
            diags.errors()
        );
    }

    /// The two entry points must agree. Same source, same verdict — the
    /// registry parameter only adds cross-module shapes, it must never change
    /// the in-module answer.
    #[test]
    fn tracked_and_imports_check_paths_agree() {
        const SRC: &str = "comptime let T: type = int\n\
                           \n\
                           def identity(x: T) -> T:\n    \
                               return x\n";
        let mut db = TycDatabase::new();
        let tracked = check_file(&mut db, "<agree>".to_owned(), SRC.to_owned());
        let registry = std::sync::Arc::new(std::collections::HashMap::new());
        let with_imports =
            check_file_with_imports(&mut db, "<agree>".to_owned(), SRC.to_owned(), &registry);
        assert_eq!(
            tracked.errors().len(),
            with_imports.errors().len(),
            "tracked path: {:?}\nimports path: {:?}",
            tracked.errors(),
            with_imports.errors()
        );
    }

    /// F56 — `set_source_text` must not touch the database when the incoming
    /// text is byte-identical. Salsa's raw `Setter::to` stamps the field as
    /// written before it looks at the value, so an unguarded re-upload
    /// invalidates every downstream memo.
    #[test]
    fn set_source_text_is_a_no_op_on_identical_text() {
        let mut db = TycDatabase::new();
        let sf = SourceFile::new(&db, "<same>".to_owned(), "let x: int = 1\n".to_owned());
        let before = module_shapes_query(&db, sf);

        let wrote = set_source_text(&mut db, sf, "let x: int = 1\n".to_owned());
        assert!(!wrote, "identical text must report no write");

        let after = module_shapes_query(&db, sf);
        assert!(
            std::sync::Arc::ptr_eq(&before.0, &after.0),
            "re-uploading identical text must leave the memo intact"
        );
    }

    /// The other half of the guard: a genuine edit must still invalidate.
    #[test]
    fn set_source_text_invalidates_on_a_real_edit() {
        let mut db = TycDatabase::new();
        let sf = SourceFile::new(
            &db,
            "<edit>".to_owned(),
            "pub class Thing:\n    a: int\n".to_owned(),
        );
        let before = module_shapes_query(&db, sf);
        assert!(!before.class_shapes["Thing"].fields.contains_key("b"));

        let wrote = set_source_text(
            &mut db,
            sf,
            "pub class Thing:\n    a: int\n    b: int\n".to_owned(),
        );
        assert!(wrote, "a real edit must report a write");

        let after = module_shapes_query(&db, sf);
        assert!(
            !std::sync::Arc::ptr_eq(&before.0, &after.0),
            "a real edit must invalidate the memo"
        );
        assert!(
            after.class_shapes["Thing"].fields.contains_key("b"),
            "the re-run must observe the new text"
        );
        // And the check pipeline sees it too, not just the shape extractor.
        let diags = check_source_file(&mut db, sf);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
    }

    // ── Name-keyed tables only describe what a name actually resolves to ──

    fn check_main(main: &str, modules: &[(&str, &str)], options: CheckOptions) -> Diagnostics {
        let registry = build_registry(modules);
        let mut db = TycDatabase::new();
        check_file_with_imports_opts(&mut db, "main.ty".into(), main.into(), &registry, options)
    }

    const GET_HELPERS: &str = "\
pub def get(table: dict[str, int], key: str, default: int) -> int:
    let found: int? = table.get(key)
    if found is not None:
        return found
    return default
";

    #[test]
    fn an_imported_function_does_not_type_a_same_named_method_call() {
        // `from helpers import get` used to arity-check `d.get(\"a\")`
        // against the imported `get`, in the importer and in `helpers`.
        let main = "\
from helpers import get

def main() -> None:
    let d: dict[str, int] = {\"a\": 1}
    let v: int? = d.get(\"a\")
    print(v, get(d, \"a\", 0))
";
        let diags = check_main(main, &[("helpers", GET_HELPERS)], CheckOptions::default());
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        let registry = build_registry(&[("helpers", GET_HELPERS)]);
        let mut db = TycDatabase::new();
        let diags =
            check_file_with_imports(&mut db, "helpers.ty".into(), GET_HELPERS.into(), &registry);
        assert!(!diags.has_errors(), "{:?}", diags.errors());
    }

    #[test]
    fn module_qualified_and_imported_calls_keep_their_arity_checks() {
        let main = "\
import helpers
from helpers import get

def main() -> None:
    let d: dict[str, int] = {\"a\": 1}
    print(helpers.get(d), get(d))
";
        let diags = check_main(main, &[("helpers", GET_HELPERS)], CheckOptions::default());
        let missing: Vec<String> = diags
            .errors()
            .iter()
            .filter(|e| {
                matches!(e, TycError::MissingArgument { missing, .. }
                    if missing.iter().any(|m| m == "key"))
            })
            .map(|e| e.to_string())
            .collect();
        assert_eq!(missing.len(), 2, "{:?}", diags.errors());
        assert!(
            missing.iter().any(|m| m.contains("`helpers.get`")),
            "{missing:?}"
        );
    }

    #[test]
    fn a_lazy_from_import_keeps_its_arity_check_on_a_315_target() {
        let helpers = "\
pub def fetch(url: str) -> str:
    return url
";
        let main = "\
lazy from helpers import fetch

def main() -> None:
    print(fetch())
";
        let diags = check_main(main, &[("helpers", helpers)], CheckOptions::for_target(15));
        assert!(
            diags.errors().iter().any(|e| matches!(
                e,
                TycError::MissingArgument { name, .. } if name == "fetch"
            )),
            "{:?}",
            diags.errors()
        );
    }

    const GEOMETRY: &str = "\
pub class Point frozen:
    x: int
    y: int

pub class Mut:
    x: int
";

    #[test]
    fn a_local_class_is_not_an_imported_frozen_class_of_the_same_name() {
        // proj_freeze / projF: `import geometry` seeds the frozen `Point`
        // under its source name; the local mutable `Point` must win.
        let main = "\
import geometry

class Point:
    x: int

class Point3(Point):
    z: int

freeze let ORIGIN = geometry.Point(x=0, y=0)

def main() -> None:
    mut p: Point = Point(x=1)
    p.x = 2
    let q: Point3 = Point3(x=1, z=2)
    print(ORIGIN.x, p.x, q.z)
";
        let diags = check_main(main, &[("geometry", GEOMETRY)], CheckOptions::default());
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        // The same with only a member import touching the module.
        let main = "\
from geometry import Mut

class Point:
    x: int

def main() -> None:
    mut p: Point = Point(x=1)
    p.x = 2
    print(p.x, Mut(x=1).x)
";
        let diags = check_main(main, &[("geometry", GEOMETRY)], CheckOptions::default());
        assert!(!diags.has_errors(), "{:?}", diags.errors());
    }

    #[test]
    fn freeze_let_judges_a_constructor_by_the_class_it_names() {
        let freezable = |value: &str| {
            let main =
                format!("import geometry\n\nclass Point:\n    x: int\n\nfreeze let V = {value}\n");
            let diags = check_main(&main, &[("geometry", GEOMETRY)], CheckOptions::default());
            !diags
                .errors()
                .iter()
                .any(|e| matches!(e, TycError::FreezeNotFreezable { .. }))
        };
        // The local mutable `Point` crashes `deep_freeze` at startup.
        assert!(!freezable("Point(x=0)"));
        // `geometry.Point` is frozen; `geometry.Mut` is not.
        assert!(freezable("geometry.Point(x=0, y=0)"));
        assert!(!freezable("geometry.Mut(x=0)"));
    }

    #[test]
    fn local_classes_win_over_imported_enums_aliases_and_newtypes() {
        // projE / projN: only a function is imported, but the touched
        // module's enum, alias and newtype were seeded by source name.
        let palette = "\
from enum import Enum

pub class Color(Enum):
    RED = 1
    BLUE = 2

pub type Id = int
pub newtype UserId = int

pub def describe(n: int) -> str:
    return str(n)
";
        let main = "\
from palette import describe

class Color:
    r: int

class Id:
    v: int

class UserId:
    v: int

def main() -> None:
    let c: Color = Color(r=1)
    let i: Id = Id(v=2)
    let u: UserId = UserId(v=3)
    print(c.r, i.v, u.v, describe(3))
";
        let diags = check_main(main, &[("palette", palette)], CheckOptions::default());
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        // Control: the imported newtype still widens where it is used.
        let main = "\
from palette import UserId

def show(n: int) -> int:
    return n

print(show(UserId(3)))
";
        let diags = check_main(main, &[("palette", palette)], CheckOptions::default());
        assert!(!diags.has_errors(), "{:?}", diags.errors());
    }

    #[test]
    fn an_imported_frozen_class_still_rejects_field_writes() {
        let main = "\
from geometry import Point

def main() -> None:
    mut p: Point = Point(x=1, y=2)
    p.x = 3
";
        let diags = check_main(main, &[("geometry", GEOMETRY)], CheckOptions::default());
        assert!(
            diags
                .errors()
                .iter()
                .any(|e| matches!(e, TycError::FrozenAssign { .. })),
            "{:?}",
            diags.errors()
        );
    }
}
