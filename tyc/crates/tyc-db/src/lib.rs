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

use rustpython_parser::{parse, Mode};
use tyc_diagnostics::{Diagnostics, TycError};
use tyc_resolve::resolve_module;
use tyc_syntax::preprocess::{
    expand_gather_blocks, expand_go_calls, expand_pipes, expand_question_ops, expand_with_chains,
    preprocess, validate_extend_usage, validate_lazy_usage, validate_question_ops,
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
/// This is the "parse-prepare" step: it strips `val`/`var` and rewrites
/// `T?` to `T | None`. Salsa caches the result, so an editor edit that
/// doesn't change the file's text content (e.g. saving with no edits)
/// avoids re-running the preprocess pass.
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
    let module = match parse(&source, Mode::Module, &path) {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
    let (resolved, _) = resolve_module(path, &source, &[], &module);
    resolved
        .module_scope()
        .bindings
        .iter()
        .map(|b| b.name.clone())
        .collect()
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
    // Currently the resolver and type-checker need the full PreprocessResult
    // (including `stripped` and `optionals` metadata), which doesn't yet
    // implement `salsa::Update` — so we run preprocess directly here. The
    // `preprocessed_text` salsa query above remains the cached entry point
    // for callers that only need the Python-compatible source string.
    let _ = SourceFile::new(db, path.clone(), text.clone());
    let mut diags = Diagnostics::new();

    // Validate `?` operator context before expanding it.  This runs on the
    // original Typhon source so it can reason about indentation-based scopes.
    // Return early on any errors: invalid `?` usage causes `expand_question_ops`
    // to inject `return` at top level, which would produce a cascading parse
    // error that obscures the real problem.
    for err in validate_question_ops(&text) {
        diags.push_error(TycError::invalid_question_op(
            err.message,
            &path,
            &text,
            err.offset,
            1,
        ));
    }
    // Reject unsupported `lazy from … import …` constructs early so the
    // downstream parser doesn't try to give a misleading diagnostic.
    for err in validate_lazy_usage(&text) {
        diags.push_error(TycError::lazy_usage(
            err.message,
            &path,
            &text,
            err.offset,
            4, // length of "lazy"
        ));
    }
    // Reject `extend BUILTIN:` declarations.  Python's built-in types cannot
    // be modified at runtime, so the silent drop performed by the impl-merge
    // desugar pass would surprise the user.
    for err in validate_extend_usage(&text) {
        diags.push_error(TycError::extend_builtin(
            err.message,
            &path,
            &text,
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
        &expand_gather_blocks(&text),
    ))));
    let prep = preprocess(&expanded);

    let module = match parse(&prep.python_source, Mode::Module, &path) {
        Ok(m) => m,
        Err(e) => {
            diags.push_error(TycError::parse(
                path,
                prep.python_source,
                e.to_string(),
                usize::from(e.offset),
            ));
            return diags;
        }
    };

    let (resolved, resolve_diags) =
        resolve_module(path.clone(), &prep.python_source, &prep.stripped, &module);
    diags.extend(resolve_diags);

    let type_diags = check_module_with(
        path,
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
        let file = SourceFile::new(&db, "<test>".to_owned(), "val x: int = 1\n".to_owned());
        let p1 = preprocessed_text(&db, file);
        let p2 = preprocessed_text(&db, file);
        assert_eq!(p1, "x: int = 1\n");
        assert_eq!(p1, p2);
    }

    #[test]
    fn module_decl_names_query() {
        let db = TycDatabase::new();
        let file = SourceFile::new(
            &db,
            "<test>".to_owned(),
            "val x: int = 1\nvar y: int = 2\ndef f() -> None:\n    pass\n".to_owned(),
        );
        let names = module_decl_names(&db, file);
        assert!(names.contains(&"x".to_owned()));
        assert!(names.contains(&"y".to_owned()));
        assert!(names.contains(&"f".to_owned()));
    }

    #[test]
    fn check_file_clean_program() {
        let mut db = TycDatabase::new();
        let diags = check_file(&mut db, "<test>".to_owned(), "val x: int = 1\n".to_owned());
        assert!(!diags.has_errors(), "{:?}", diags.errors());
    }

    #[test]
    fn check_file_reports_type_mismatch() {
        let mut db = TycDatabase::new();
        let diags = check_file(
            &mut db,
            "<test>".to_owned(),
            "val x: int = \"hi\"\n".to_owned(),
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
    val x: int = \"hi\"
";
        let diags = check_file(&mut db, "<test>".to_owned(), src.to_owned());
        assert!(
            !diags.has_errors(),
            "unsafe block should suppress type errors; got {:?}",
            diags.errors()
        );
    }

    #[test]
    fn check_file_rejects_extend_on_builtin_str() {
        let mut db = TycDatabase::new();
        let src = "extend str:\n    def slug(self) -> str: return self\n";
        let diags = check_file(&mut db, "<test>".to_owned(), src.to_owned());
        assert!(
            diags.has_errors(),
            "extend on a built-in type must produce a diagnostic"
        );
        let msg = format!("{:?}", diags.errors()[0]);
        assert!(
            msg.contains("extend str") || msg.contains("built-in"),
            "diagnostic should mention the rejected builtin; got {msg}"
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
val outer: int = \"oops\"
unsafe:
    val inner: int = \"hi\"
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

val greeting: str = \"Hello from Typhon!\"

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
            "val x: int = 1\nx = 2\n".to_owned(),
        );
        assert!(diags.has_errors());
    }

    // ── ? operator context enforcement ──────────────────────────────────────

    #[test]
    fn check_file_question_op_valid_in_result_fn() {
        let src = "\
def parse(s: str) -> Result[int, str]:
    val n = int(s)?
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
        let src = "val x = load()?\n";
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
        let src = "def run() -> None:\n    val x = fetch()?\n";
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
        let src = "import os\nval x: int = 1\n";
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
        let src = "import os\nval sep: str = os.sep\n";
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

val p: Point = Point()
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

val u: User = User()
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
comptime val PORT: int = 8080

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
}
