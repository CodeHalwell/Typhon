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
use tyc_resolve::{resolve_module, ResolvedModule};
use tyc_syntax::{
    parse_module,
    preprocess::{
        expand_gather_blocks, expand_go_calls, expand_pipes, expand_question_ops,
        expand_with_chains, preprocess, validate_extend_usage, validate_lazy_usage,
        validate_question_ops,
    },
};
use tyc_types::check_module_with;

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
    let text = file.text(db);
    // Apply Typhon sugar expansion in the same order as `check_file` and the
    // build pipeline: gather → go → with-chains → pipes → `?`.
    let expanded = expand_question_ops(&expand_pipes(&expand_with_chains(&expand_go_calls(
        &expand_gather_blocks(text),
    ))));
    preprocess(&expanded).python_source
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
    let source = preprocessed_text(db, file);
    let path = file.path(db).clone();
    let parsed = match parse_module(&source) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let module = parsed.into_syntax();
    let (resolved, _) = resolve_module(path, &source, &module);
    resolved
        .module_scope()
        .bindings
        .iter()
        .map(|b| b.name.clone())
        .collect()
}

/// Newtype wrapper around `Arc<ResolvedModule>` so we can implement
/// `salsa::Update` for it without violating the orphan rule.
///
/// Salsa requires the return type of a `#[salsa::tracked]` query to implement
/// `Update`.  `ResolvedModule` contains `Vec`s of structs that don't implement
/// `PartialEq`, so we use pointer comparison.  Salsa only calls `maybe_update`
/// after the query body has already re-run (i.e. when an input changed), so
/// the conservative "always-changed" strategy is correct.
#[derive(Clone)]
pub struct ArcResolvedModule(pub Arc<ResolvedModule>);

impl PartialEq for ArcResolvedModule {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
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
// Arc (decrementing its refcount) before storing the new one, which is correct.
// Pointer equality is used as a conservative proxy for value equality.
unsafe impl salsa::Update for ArcResolvedModule {
    unsafe fn maybe_update(old_pointer: *mut Self, new_value: Self) -> bool {
        if Arc::ptr_eq(&(*old_pointer).0, &new_value.0) {
            false
        } else {
            *old_pointer = new_value;
            true
        }
    }
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
    let source = preprocessed_text(db, file);
    let path = file.path(db).clone();
    match parse_module(&source) {
        Ok(parsed) => {
            let module = parsed.into_syntax();
            let (resolved, _) = resolve_module(path, &source, &module);
            ArcResolvedModule(Arc::new(resolved))
        }
        Err(_) => ArcResolvedModule(Arc::new(ResolvedModule::default())),
    }
}

/// Convenience alias — extract the inner `Arc<ResolvedModule>` from a
/// `resolved_module` query result.
pub fn resolved_module_arc(db: &dyn salsa::Database, file: SourceFile) -> Arc<ResolvedModule> {
    resolved_module(db, file).0
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
/// Uses the salsa db for the cacheable preprocess step. The resolve and
/// type-check passes run directly because their outputs don't yet
/// implement `salsa::Update`; they will be moved under salsa in later
/// phases.
pub fn check_file(db: &mut TycDatabase, path: String, text: String) -> Diagnostics {
    let _ = SourceFile::new(db, path.clone(), text.clone());
    check_impl(&path, &text)
}

/// Like [`check_file`] but uses a caller-supplied [`SourceFile`] handle.
///
/// The handle must already exist in `db` (created via [`SourceFile::new`] or
/// updated via `source_file.set_text(&mut db).to(text)`).  The LSP uses this
/// variant so it can retain the handle across `did_open`/`did_change` events
/// and then call [`preprocessed_text`] from hover/definition handlers; Salsa
/// serves the preprocessed source from cache when the file has not changed.
pub fn check_source_file(db: &mut TycDatabase, source_file: SourceFile) -> Diagnostics {
    let path = source_file.path(db).clone();
    let text = source_file.text(db).clone();
    check_impl(&path, &text)
}

/// Shared check implementation used by both [`check_file`] and
/// [`check_source_file`].
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
    let expanded = expand_question_ops(&expand_pipes(&expand_with_chains(&expand_go_calls(
        &expand_gather_blocks(text),
    ))));
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

    let (resolved, resolve_diags) = resolve_module(path.to_owned(), &prep.python_source, &module);
    diags.extend(resolve_diags);

    let type_diags = check_module_with(
        path.to_owned(),
        &prep.python_source,
        &resolved,
        &module,
        &prep.unsafe_lines,
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

let p: Point = Point()
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

let u: User = User()
";
        let mut db = TycDatabase::new();
        let diags = check_file(&mut db, "<test>".into(), src.to_owned());
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
}
