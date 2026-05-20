//! `tyc check` — parse, resolve, and type-check without emitting code.
//!
//! Runs the full Phase-1 pipeline: pre-process → parse → resolve
//! (scopes + `val`/`var` enforcement + unknown-name diagnostics) → type
//! check (nominal types, non-null narrowing, argument-count and
//! argument-type checks). Diagnostics are rendered via miette.

use std::path::PathBuf;

use clap::Args;
use miette::{miette, Result};

use tyc_analyse::{analyse_purity, evaluate_comptime_with_functions, purity_diagnostics};
use tyc_db::{check_file, TycDatabase};
use tyc_diagnostics::{Diagnostics, TycError};
use tyc_emit::{compare_modules, StubTestKind};
use tyc_resolve::check_unknown_modules;
use tyc_syntax::preprocess::{
    expand_gather_blocks, expand_go_calls, expand_lazy_imports, expand_multiline_guards,
    expand_pipes, expand_question_ops, expand_with_chains, preprocess,
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

    // FINDINGS #79: build the set of dotted module names contained in the
    // project so the per-file unknown-module check can resolve sibling
    // imports without falsely flagging them. `extra_modules` adds
    // typhon.toml-declared dependencies; users who manage deps directly
    // through `uv`/`pip` can still bypass the check by listing the
    // package in `typhon.toml`.
    let project_modules = collect_project_modules(&args.paths, &config_start);
    let extra_modules: Vec<String> = config
        .dependencies
        .keys()
        .chain(config.dev_dependencies.keys())
        .cloned()
        .collect();

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

            let file_diags = check_file(&mut db, path.display().to_string(), source.clone());
            diags.extend(file_diags);

            // Run comptime + purity analysis to match what `tyc build` would
            // reject. Without this pass, CI pipelines running only `tyc check`
            // would silently accept `@pure` violations and unsatisfied
            // required env vars that production builds catch. The work
            // duplicates `tyc build`'s phase 2/3 setup; salsa caches the
            // preprocess so it's cheap on warm runs.
            let analysis_diags = run_analysis_passes(&path.display().to_string(), &source);
            diags.extend(analysis_diags);

            // FINDINGS #79: vet imports against stdlib + project + deps.
            let module_diags = run_unknown_module_check(
                &path.display().to_string(),
                &source,
                &project_modules,
                &extra_modules,
            );
            diags.extend(module_diags);
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
                let file_diags = check_file(&mut db, path.display().to_string(), source.clone());
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
    let mut diags = apply_strictness(diags, &config);
    // Remove duplicate diagnostics that can arise when multiple files share
    // an error root (e.g. a repeated definition across passes).
    diags.dedup();

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

/// Run comptime evaluation and purity verification on a single source file.
///
/// These passes also run inside `tyc build`; lifting them up to `tyc check`
/// closes the documented CI hole where `@pure` violations and missing
/// `[env] required` variables only fail at build time. Any non-comptime,
/// non-purity error has already been reported by `check_file`, so this
/// helper deliberately swallows preprocess / parse failures (they would
/// surface a second time otherwise).
fn run_analysis_passes(path: &str, source: &str) -> Diagnostics {
    let mut diags = Diagnostics::new();
    let expanded = expand_question_ops(&expand_pipes(&expand_with_chains(&expand_go_calls(
        &expand_gather_blocks(&expand_lazy_imports(source)),
    ))));
    let prep = preprocess(&expanded);
    let module = match tyc_syntax::parse_module(&prep.python_source) {
        Ok(p) => p.into_syntax(),
        Err(_) => return diags,
    };

    // Pass `comptime_functions` so `comptime def` calls dispatch
    // correctly in check the same way they do in build (FINDINGS #48).
    let (_, comptime_diags) = evaluate_comptime_with_functions(
        &module,
        &prep.comptime_bindings,
        &prep.comptime_functions,
    );
    diags.extend(comptime_diags);

    let purity_findings = analyse_purity(&module, false);
    let purity_diags = purity_diagnostics(&purity_findings, path, source);
    diags.extend(purity_diags);

    diags
}

/// Collect the dotted-name form of every `.ty` file under the given
/// `paths`, relative to `config_start` (which is typically the project
/// root). Used by [`run_unknown_module_check`] to vet sibling imports.
///
/// Names are derived by stripping the path prefix up to the first `src/`
/// component (matching Typhon's standard layout) and joining the remaining
/// segments with `.`. `src/main.ty` becomes `"main"`; `src/pkg/sub.ty`
/// becomes `"pkg.sub"`; an `__init__.ty` collapses to its parent package
/// name. Files outside any `src/` directory fall back to their basename
/// so single-file scripts still resolve correctly.
fn collect_project_modules(
    paths: &[PathBuf],
    _config_start: &std::path::Path,
) -> Vec<String> {
    let mut modules: Vec<String> = Vec::new();
    for root in paths {
        if let Ok(files) = collect_ty_files(root) {
            for file in files {
                let dotted = ty_path_to_dotted(&file);
                if !modules.contains(&dotted) {
                    modules.push(dotted);
                }
            }
        }
    }
    modules
}

fn ty_path_to_dotted(path: &std::path::Path) -> String {
    let components: Vec<String> = path
        .with_extension("")
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(os) => Some(os.to_string_lossy().to_string()),
            _ => None,
        })
        .collect();
    // Trim leading "src" so `src/main.ty` becomes `main`. Anything before
    // `src` (an absolute path's leading directories) is dropped so the
    // dotted-name reflects the package layout, not the filesystem layout.
    let src_idx = components.iter().rposition(|c| c == "src");
    let tail: Vec<&str> = match src_idx {
        Some(i) => components[i + 1..].iter().map(|s| s.as_str()).collect(),
        None => components
            .last()
            .map(|s| vec![s.as_str()])
            .unwrap_or_default(),
    };
    // Drop trailing `__init__` so a package directory is named by its
    // folder, not the init module.
    let mut tail = tail;
    if tail.last().is_some_and(|s| *s == "__init__") {
        tail.pop();
    }
    tail.join(".")
}

/// FINDINGS #79: run `check_unknown_modules` for one source file. Parses
/// the file through the same preprocess pipeline used elsewhere so
/// Typhon-specific keywords (`val`, `var`, `lazy import`, …) are stripped
/// before the AST walk. Returns the warnings only — errors at the
/// preprocess / parse layer have already been surfaced by `check_file`.
fn run_unknown_module_check(
    path: &str,
    source: &str,
    project_modules: &[String],
    extra_modules: &[String],
) -> Diagnostics {
    let expanded = expand_question_ops(&expand_pipes(&expand_with_chains(&expand_go_calls(
        &expand_gather_blocks(&expand_multiline_guards(&expand_lazy_imports(source))),
    ))));
    let prep = preprocess(&expanded);
    let module = match tyc_syntax::parse_module(&prep.python_source) {
        Ok(p) => p.into_syntax(),
        Err(_) => return Diagnostics::new(),
    };
    check_unknown_modules(path, source, &module, project_modules, extra_modules)
}

/// Run the full preprocess + parse pipeline on `source` and return the
/// resulting Python AST.  Used by the stub diff so that Typhon-specific
/// syntax (`val`, `var`, `model`, `interface`, `extend`, sugar passes)
/// is normalised before comparing.
fn parse_for_diff(source: &str) -> Result<ruff_python_ast::ModModule> {
    let expanded = expand_question_ops(&expand_pipes(&expand_with_chains(&expand_go_calls(
        &expand_gather_blocks(&expand_lazy_imports(source)),
    ))));
    let prep = preprocess(&expanded);
    tyc_syntax::parse_module(&prep.python_source)
        .map(|p| p.into_syntax())
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
        write_ty(tmp.path(), "ok.ty", "let x: int = 1\n");
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: false,
        };
        run(args).unwrap();
    }

    #[test]
    fn check_reports_type_mismatch_as_error() {
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "bad.ty", "let x: int = \"hello\"\n");
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: false,
        };
        assert!(run(args).is_err(), "type mismatch should be an error");
    }

    #[test]
    fn check_passes_nullable_annotation() {
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "nullable.ty", "let x: str? = None\n");
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: false,
        };
        run(args).unwrap();
    }

    #[test]
    fn check_reports_val_reassignment_as_error() {
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "immut.ty", "let x: int = 1\nx = 2\n");
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: false,
        };
        assert!(run(args).is_err(), "val reassignment should be an error");
    }

    #[test]
    fn check_reports_frozen_field_write_as_error() {
        // End-to-end guard for the user-visible flow: `tyc check` must
        // surface `tyc::frozen_assign` instead of letting the program
        // build cleanly and crash at runtime with `FrozenInstanceError`.
        let tmp = tempfile::tempdir().unwrap();
        write_ty(
            tmp.path(),
            "frozen.ty",
            "\
class Identity frozen:
    name: str

let i: Identity = Identity(name=\"Alice\")
i.name = \"Bob\"
",
        );
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: false,
        };
        assert!(
            run(args).is_err(),
            "writes to a frozen class field must fail `tyc check`"
        );
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
        write_ty(
            tmp.path(),
            "lib.ty",
            "def goodbye(name: str) -> str:\n    return name\n",
        );
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
