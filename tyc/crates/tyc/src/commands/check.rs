//! `tyc check` — parse, resolve, and type-check without emitting code.
//!
//! Runs the full Phase-1 pipeline: pre-process → parse → resolve
//! (scopes + `val`/`var` enforcement + unknown-name diagnostics) → type
//! check (nominal types, non-null narrowing, argument-count and
//! argument-type checks). Diagnostics are rendered via miette.

use std::path::PathBuf;

use clap::Args;
use miette::{miette, Result};
use rustpython_parser::{parse, Mode};

use tyc_db::{check_file, TycDatabase};
use tyc_diagnostics::{Diagnostics, TycError};
use tyc_emit::{compare_modules, StubTestKind};
use tyc_syntax::preprocess::{
    expand_gather_blocks, expand_go_calls, expand_lazy_imports, expand_pipes, expand_question_ops,
    expand_with_chains, preprocess,
};

use crate::commands::util::{apply_strictness, collect_dty_files, collect_ty_files};
use crate::config::TyphonConfig;

/// Arguments for `tyc check`.
#[derive(Args, Debug)]
pub struct CheckArgs {
    /// `.ty` files or directories to check.
    #[arg(value_name = "PATH", default_value = ".")]
    pub paths: Vec<PathBuf>,

    /// Validate `.dty` stub files against the runtime modules they describe.
    ///
    /// In v1 this flag only validates that every `.dty` file parses, resolves,
    /// and type-checks cleanly — the full stubtest-style runtime comparison
    /// (which would import the implementation module and diff its symbols) is
    /// deferred. The flag is recognised today so CI configurations are
    /// forward-compatible.
    #[arg(long)]
    pub stubs: bool,
}

pub fn run(args: CheckArgs) -> Result<()> {
    // Load strictness config from `typhon.toml`, anchoring the search to the
    // first checked path (or CWD when none is provided) so that
    // `tyc check path/to/project` uses that project's config, not the caller's.
    let config_start = args
        .paths
        .first()
        .map(|p| {
            if p.is_file() {
                p.parent()
                    .map(|d| d.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."))
            } else {
                p.clone()
            }
        })
        .unwrap_or_else(|| PathBuf::from("."));
    let config = match TyphonConfig::load(&config_start) {
        Ok(Some((_, cfg))) => cfg,
        Ok(None) => TyphonConfig::default(),
        Err(e) => return Err(miette!("{e}")),
    };

    let mut diags = Diagnostics::new();
    let mut file_count = 0usize;
    let mut db = TycDatabase::new();

    for root in &args.paths {
        for path in collect_ty_files(root)? {
            file_count += 1;

            let source = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    diags.push_error(TycError::io(path.display().to_string(), &e));
                    continue;
                }
            };

            let file_diags = check_file(&mut db, path.display().to_string(), source);
            diags.extend(file_diags);
        }

        // `--stubs`: parse + type-check every `.dty` stub, then compare its
        // surface API against the sibling `.ty` (or `.py`) implementation
        // module.  Mismatches are reported through the standard diagnostics
        // channel so CI treats them like any other check error.
        if args.stubs {
            for path in collect_dty_files(root)? {
                file_count += 1;
                let source = match std::fs::read_to_string(&path) {
                    Ok(s) => s,
                    Err(e) => {
                        diags.push_error(TycError::io(path.display().to_string(), &e));
                        continue;
                    }
                };
                let file_diags =
                    check_file(&mut db, path.display().to_string(), source.clone());
                diags.extend(file_diags);

                // Find the implementation module by stem.  Prefer a sibling
                // `.ty` (Typhon source) over a `.py` (raw Python) so users
                // can stub a Typhon module without also writing Python.
                let impl_path = path
                    .with_extension("ty")
                    .exists()
                    .then(|| path.with_extension("ty"))
                    .or_else(|| {
                        let py = path.with_extension("py");
                        py.exists().then_some(py)
                    });
                let Some(impl_path) = impl_path else {
                    // No implementation file located — stub stands alone, no
                    // diff possible.  This is intentional for downstream
                    // libraries the user is describing for type-checkers.
                    continue;
                };
                let impl_source = match std::fs::read_to_string(&impl_path) {
                    Ok(s) => s,
                    Err(e) => {
                        diags.push_error(TycError::io(impl_path.display().to_string(), &e));
                        continue;
                    }
                };

                match diff_stub_against_impl(&source, &impl_source) {
                    Ok(findings) => {
                        for finding in findings {
                            let label = match finding.kind {
                                StubTestKind::MissingInImpl => "missing in implementation",
                                StubTestKind::MissingInStub => "missing in stub",
                                StubTestKind::SignatureMismatch => "signature mismatch",
                            };
                            diags.push_error(TycError::stub_mismatch(
                                format!("{label}: {}", finding.message),
                                path.display().to_string(),
                                source.clone(),
                                0,
                                1,
                            ));
                        }
                    }
                    Err(e) => {
                        diags.push_error(TycError::stub_mismatch(
                            format!("could not diff stub against implementation: {e}"),
                            path.display().to_string(),
                            source.clone(),
                            0,
                            1,
                        ));
                    }
                }
            }
        }
    }

    // Apply strictness rules (e.g. promote unused-import warnings to errors).
    let diags = apply_strictness(diags, &config);

    // Emit warnings regardless of whether there are errors.
    for warn in diags.warnings() {
        eprintln!("{:?}", miette::Report::new_boxed(Box::new(warn.clone())));
    }

    if diags.has_errors() {
        for err in diags.errors() {
            eprintln!("{:?}", miette::Report::new_boxed(Box::new(err.clone())));
        }
        return Err(miette!(
            "{} error(s) in {} file(s)",
            diags.error_count(),
            file_count
        ));
    }

    if diags.warning_count() > 0 {
        println!(
            "checked {} file(s) — {} warning(s)",
            file_count,
            diags.warning_count()
        );
    } else {
        println!("checked {} file(s) — no errors", file_count);
    }
    Ok(())
}

/// Run the full preprocess + parse pipeline on `source` and return the
/// resulting Python AST.  Used by the stub diff so that Typhon-specific
/// syntax (`val`, `var`, `model`, `interface`, `extend`, sugar passes)
/// is normalised before comparing.
fn parse_for_diff(source: &str) -> Result<rustpython_ast::Mod<rustpython_ast::text_size::TextRange>>
{
    let expanded = expand_question_ops(&expand_pipes(&expand_with_chains(&expand_go_calls(
        &expand_gather_blocks(&expand_lazy_imports(source)),
    ))));
    let prep = preprocess(&expanded);
    parse(&prep.python_source, Mode::Module, "<stubtest>")
        .map_err(|e| miette!("parse error: {e}"))
}

fn diff_stub_against_impl(
    stub_source: &str,
    impl_source: &str,
) -> Result<Vec<tyc_emit::StubTestFinding>> {
    let stub = parse_for_diff(stub_source)?;
    let imp = parse_for_diff(impl_source)?;
    Ok(compare_modules(&stub, &imp))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_ty(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn check_passes_valid_ty_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "ok.ty", "val x: int = 1\n");
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: false,
        };
        run(args).unwrap();
    }

    #[test]
    fn check_reports_type_mismatch_as_error() {
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "bad.ty", "val x: int = \"hello\"\n");
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: false,
        };
        assert!(run(args).is_err(), "type mismatch should be an error");
    }

    #[test]
    fn check_passes_nullable_annotation() {
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "nullable.ty", "val x: str? = None\n");
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: false,
        };
        run(args).unwrap();
    }

    #[test]
    fn check_reports_val_reassignment_as_error() {
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "immut.ty", "val x: int = 1\nx = 2\n");
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: false,
        };
        assert!(run(args).is_err(), "val reassignment should be an error");
    }

    #[test]
    fn check_stubs_passes_for_matching_stub_and_impl() {
        let tmp = tempfile::tempdir().unwrap();
        // The stub and implementation expose the same function with the
        // same parameter names — diff should be empty.
        write_ty(tmp.path(), "lib.dty", "def hello(name: str) -> str: ...\n");
        write_ty(
            tmp.path(),
            "lib.ty",
            "def hello(name: str) -> str:\n    return name\n",
        );
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: true,
        };
        run(args).unwrap();
    }

    #[test]
    fn check_stubs_fails_when_function_missing_in_impl() {
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "lib.dty", "def hello(name: str) -> str: ...\n");
        write_ty(tmp.path(), "lib.ty", "def goodbye(name: str) -> str:\n    return name\n");
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: true,
        };
        assert!(
            run(args).is_err(),
            "stub declares hello() which impl does not — should error"
        );
    }

    #[test]
    fn check_stubs_fails_when_param_names_differ() {
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "lib.dty", "def hello(name: str) -> str: ...\n");
        write_ty(
            tmp.path(),
            "lib.ty",
            "def hello(other_name: str) -> str:\n    return other_name\n",
        );
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: true,
        };
        assert!(
            run(args).is_err(),
            "parameter rename should produce a signature-mismatch error"
        );
    }
}
