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
use tyc_desugar::{analyse_module, ClassDefault};
use tyc_diagnostics::{Diagnostics, TycError};
use tyc_emit::{emit_compiled, EmitOptions};
use tyc_resolve::resolve_module;
use tyc_syntax::preprocess::preprocess;
use tyc_types::check_module;

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
pub fn preprocessed_text<'db>(db: &'db dyn salsa::Database, file: SourceFile) -> String {
    let text = file.text(db);
    preprocess(text).python_source
}

/// Tracked query: the names declared at the top level of the module.
///
/// This is a cheap proxy for "module resolution": it parses the
/// preprocessed source and returns the list of top-level binding names.
/// The full [`ResolvedModule`](tyc_resolve::ResolvedModule) isn't yet
/// `salsa::Update`-friendly, so this is the slice of the resolve step
/// that's salsa-cacheable today.
#[salsa::tracked]
pub fn module_decl_names<'db>(db: &'db dyn salsa::Database, file: SourceFile) -> Vec<String> {
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

/// Output of the full build pipeline for a single file.
pub struct BuildOutput {
    pub diagnostics: Diagnostics,
    /// The emitted Python source, or `None` if there were type errors.
    pub python_source: Option<String>,
}

/// Full build pipeline for a single `.ty` file.
///
/// Runs preprocess → parse → resolve → type-check → desugar → emit.
/// On success returns the emitted Python source. On type errors returns
/// `None` for the source (diagnostics still populated).
pub fn build_file(
    _db: &mut TycDatabase,
    path: String,
    text: String,
    class_default: ClassDefault,
) -> BuildOutput {
    let prep = preprocess(&text);

    let module = match parse(&prep.python_source, Mode::Module, &path) {
        Ok(m) => m,
        Err(e) => {
            let mut diags = Diagnostics::new();
            diags.push_error(TycError::parse(
                path,
                prep.python_source,
                e.to_string(),
                usize::from(e.offset),
            ));
            return BuildOutput {
                diagnostics: diags,
                python_source: None,
            };
        }
    };

    let (resolved, resolve_diags) =
        resolve_module(path.clone(), &prep.python_source, &prep.stripped, &module);
    let type_diags = check_module(path, &prep.python_source, &resolved, &module);

    let mut diags = Diagnostics::new();
    diags.extend(resolve_diags);
    diags.extend(type_diags);

    if diags.has_errors() {
        return BuildOutput {
            diagnostics: diags,
            python_source: None,
        };
    }

    let desugar_info = analyse_module(&module, class_default);
    let options = EmitOptions {
        inject_dataclass: desugar_info.needs_dataclass_import,
        inject_pydantic: desugar_info.needs_pydantic_import,
    };
    let python = emit_compiled(&module, &options);

    BuildOutput {
        diagnostics: diags,
        python_source: Some(python),
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

    let prep = preprocess(&text);

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

    let type_diags = check_module(path, &prep.python_source, &resolved, &module);
    diags.extend(type_diags);

    diags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preprocessed_text_query_caches() {
        let mut db = TycDatabase::new();
        let file = SourceFile::new(&mut db, "<test>".to_owned(), "val x: int = 1\n".to_owned());
        let p1 = preprocessed_text(&db, file);
        let p2 = preprocessed_text(&db, file);
        assert_eq!(p1, "x: int = 1\n");
        assert_eq!(p1, p2);
    }

    #[test]
    fn module_decl_names_query() {
        let mut db = TycDatabase::new();
        let file = SourceFile::new(
            &mut db,
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
        let diags = check_file(&mut db, "<test>".to_owned(), "val x: int = \"hi\"\n".to_owned());
        assert!(diags.has_errors());
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
}
