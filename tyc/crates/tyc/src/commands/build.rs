//! `tyc build` — full compilation pipeline.
//!
//! Runs: expand `?` operators → pre-process → parse → type-check →
//!       evaluate comptime → substitute literals → desugar → emit.
//! Writes `.py` files into the output directory, mirroring the source tree.

use std::collections::HashMap;
use std::path::PathBuf;

use clap::Args;
use miette::{miette, Result};
use rustpython_ast::{text_size::TextRange, Constant, Expr, ExprConstant, Mod, Stmt};
use rustpython_parser::{parse, Mode};

use tyc_analyse::{evaluate_comptime, ComptimeValue};
use tyc_db::{check_file, TycDatabase};
use tyc_desugar::desugar_module;
use tyc_diagnostics::Diagnostics;
use tyc_emit::emit;
use tyc_format::format_source;
use tyc_syntax::preprocess::{
    expand_lazy_imports, expand_pipes, expand_question_ops, expand_with_chains, preprocess,
};

use crate::commands::util::{apply_strictness, collect_ty_files};
use crate::config::TyphonConfig;

/// Arguments for `tyc build`.
#[derive(Args, Debug)]
pub struct BuildArgs {
    /// Project directory (defaults to current directory).
    #[arg(value_name = "PATH", default_value = ".")]
    pub path: PathBuf,

    /// Write output to this directory instead of the configured `out` dir.
    /// Relative paths are resolved against the project root.
    #[arg(long, short = 'o', value_name = "DIR")]
    pub out: Option<PathBuf>,

    /// Skip formatting the emitted Python.
    #[arg(long)]
    pub no_format: bool,
}

pub fn run(args: BuildArgs) -> Result<()> {
    let project_root = args
        .path
        .canonicalize()
        .map_err(|e| miette!("cannot resolve path '{}': {}", args.path.display(), e))?;

    // Load typhon.toml, anchoring src/out to the directory that contains it
    // so that `tyc build` works correctly when invoked from a subdirectory.
    let (config_dir, config) = match TyphonConfig::load(&project_root) {
        Ok(Some((toml_path, cfg))) => {
            let dir = toml_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| project_root.clone());
            (dir, cfg)
        }
        Ok(None) => {
            eprintln!("warning: no typhon.toml found; using defaults");
            (project_root.clone(), TyphonConfig::default())
        }
        Err(e) => return Err(miette!("{e}")),
    };

    let src_dir = config_dir.join(&config.project.src);

    // Resolve --out relative to project_root so `tyc build path/to/proj -o build`
    // writes to `path/to/proj/build` rather than the caller's cwd.
    let out_dir = match args.out {
        Some(out) => {
            if out.is_absolute() {
                out
            } else {
                project_root.join(out)
            }
        }
        None => config_dir.join(&config.project.out),
    };

    let do_format = config.emit.format && !args.no_format;

    // Fail fast if any required env vars are missing (declared in [env] required).
    for var in &config.env.required {
        if std::env::var(var).is_err() {
            return Err(miette!(
                "required environment variable '{}' is not set \
                 (declared in [env] required in typhon.toml)",
                var
            ));
        }
    }

    if !src_dir.exists() {
        return Err(miette!(
            "source directory '{}' does not exist",
            src_dir.display()
        ));
    }

    let ty_files = collect_ty_files(&src_dir)?;

    if ty_files.is_empty() {
        println!("no .ty files found in '{}'", src_dir.display());
        return Ok(());
    }

    // Read every source file once; both phases reuse this buffer.
    let sources: Vec<(PathBuf, String)> = ty_files
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| miette!("cannot read '{}': {e}", path.display()))?;
            Ok((path, text))
        })
        .collect::<Result<_>>()?;

    // Phase 1: type-check all files first and fail fast on errors.
    let mut db = TycDatabase::new();
    let mut all_phase1_diags = Diagnostics::new();

    for (path, source) in &sources {
        let file_diags = check_file(&mut db, path.display().to_string(), source.clone());
        all_phase1_diags.extend(file_diags);
    }

    // Apply strictness rules (e.g. promote unused-import warnings to errors).
    let all_phase1_diags = apply_strictness(all_phase1_diags, &config);

    // Emit warnings even when there are no errors so they are always visible.
    for warn in all_phase1_diags.warnings() {
        eprintln!("{:?}", miette::Report::new_boxed(Box::new(warn.clone())));
    }

    if all_phase1_diags.has_errors() {
        for err in all_phase1_diags.errors() {
            eprintln!("{:?}", miette::Report::new_boxed(Box::new(err.clone())));
        }
        return Err(miette!(
            "{} error(s) — fix type errors before building",
            all_phase1_diags.error_count()
        ));
    }

    // Phase 2: desugar and emit using the already-loaded source text.
    let mut emitted = 0usize;
    let mut needs_runtime = false;

    for (path, source) in &sources {
        // Expand Typhon syntactic sugar in order:
        //   1. `with`-chains lower to a flat sequence of guarded unwraps,
        //   2. pipe operators rewrite `a |> f(b)` to `f(a, b)`,
        //   3. the `?` operator unwraps any remaining `Result`-typed calls.
        // After this the Python parser only sees standard Python plus the
        // `val`/`var`/`model`/`impl`/`comptime` keywords stripped by
        // `preprocess`.
        let expanded = expand_question_ops(&expand_pipes(&expand_with_chains(
            &expand_lazy_imports(source),
        )));
        let prep = preprocess(&expanded);

        let module = parse(
            &prep.python_source,
            Mode::Module,
            &path.display().to_string(),
        )
        .map_err(|e| miette!("parse error in '{}': {e}", path.display()))?;

        // Evaluate all `comptime` bindings and substitute their literals into
        // the AST before desugaring.
        let (comptime_values, comptime_diags) = evaluate_comptime(&module, &prep.comptime_bindings);
        if comptime_diags.has_errors() {
            for err in comptime_diags.errors() {
                eprintln!("{:?}", miette::Report::new_boxed(Box::new(err.clone())));
            }
            return Err(miette!(
                "{} comptime error(s) in '{}'",
                comptime_diags.error_count(),
                path.display()
            ));
        }

        let module = substitute_comptime_literals(module, &comptime_values);

        let desugar_output = desugar_module(&module);
        if desugar_output.needs_typhon_runtime {
            needs_runtime = true;
        }
        let mut python_src = emit(&desugar_output.module);

        // Optionally normalise whitespace in the emitted Python (tabs → spaces,
        // trailing whitespace, final newline).  Full ruff-style reformatting
        // will replace this when the ruff vendor fork lands in Phase 3.
        if do_format {
            let path_str = path.display().to_string();
            if let Ok(result) = format_source(&python_src, &path_str) {
                python_src = result.output;
            }
        }

        let rel = path
            .strip_prefix(&src_dir)
            .map_err(|_| miette!("'{}' is outside the source directory", path.display()))?;
        let out_file = out_dir.join(rel).with_extension("py");

        if let Some(parent) = out_file.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| miette!("cannot create '{}': {e}", parent.display()))?;
        }

        std::fs::write(&out_file, &python_src)
            .map_err(|e| miette!("cannot write '{}': {e}", out_file.display()))?;

        emitted += 1;
    }

    // Emit the typhon_runtime helper alongside the Python output when any
    // source file uses Ok, Err, or Result.  The helper is a generated module
    // the build owns; users do not need to install a separate package.
    if needs_runtime {
        std::fs::create_dir_all(&out_dir)
            .map_err(|e| miette!("cannot create output dir '{}': {e}", out_dir.display()))?;
        let runtime_path = out_dir.join("typhon_runtime.py");
        std::fs::write(&runtime_path, TYPHON_RUNTIME_PY)
            .map_err(|e| miette!("cannot write '{}': {e}", runtime_path.display()))?;
        println!("wrote typhon_runtime.py → '{}'", runtime_path.display());
    }

    println!("built {} file(s) → '{}'", emitted, out_dir.display());
    Ok(())
}

// ── Comptime literal substitution ─────────────────────────────────────────────

/// Replace the RHS of every top-level annotated assignment whose name appears
/// in `values` with the evaluated compile-time constant.
///
/// This transforms e.g.:
/// ```python
/// PORT: int = int(env("PORT", "8080"))
/// ```
/// into:
/// ```python
/// PORT: int = 8080
/// ```
fn substitute_comptime_literals(
    module: Mod<TextRange>,
    values: &HashMap<String, ComptimeValue>,
) -> Mod<TextRange> {
    if values.is_empty() {
        return module;
    }
    let Mod::Module(mut m) = module else {
        return module;
    };
    m.body = m
        .body
        .into_iter()
        .map(|stmt| substitute_stmt(stmt, values))
        .collect();
    Mod::Module(m)
}

fn substitute_stmt(
    stmt: Stmt<TextRange>,
    values: &HashMap<String, ComptimeValue>,
) -> Stmt<TextRange> {
    if let Stmt::AnnAssign(mut ann) = stmt {
        if let Expr::Name(ref n) = *ann.target {
            if let Some(cv) = values.get(n.id.as_str()) {
                ann.value = Some(Box::new(comptime_value_to_expr(cv)));
                return Stmt::AnnAssign(ann);
            }
        }
        Stmt::AnnAssign(ann)
    } else {
        stmt
    }
}

/// Convert a [`ComptimeValue`] to its Python AST constant expression
/// by round-tripping through the Python parser.
fn comptime_value_to_expr(value: &ComptimeValue) -> Expr<TextRange> {
    let literal = value.to_python_literal();
    match parse(&literal, Mode::Expression, "<comptime>") {
        Ok(Mod::Expression(e)) => *e.body,
        _ => {
            // Fallback: emit as a string-quoted constant.  Should never happen
            // for the value types we produce.
            Expr::Constant(ExprConstant {
                range: TextRange::default(),
                value: Constant::Str(literal),
                kind: None,
            })
        }
    }
}

/// Generated `typhon_runtime.py` — the minimal runtime helper for `Result`.
///
/// Provides `Ok[T]`, `Err[E]`, and `Result[T, E]` as dataclass-based types.
/// This file is emitted into the project's build output directory whenever a
/// `.ty` source file references any of these names, so no separate PyPI
/// package is required to deploy a Typhon project.
const TYPHON_RUNTIME_PY: &str = "\
# generated by tyc — do not edit
from __future__ import annotations

from dataclasses import dataclass
from typing import Generic, TypeVar

_T = TypeVar(\"_T\")
_E = TypeVar(\"_E\")


@dataclass(slots=True)
class Ok(Generic[_T]):
    value: _T


@dataclass(slots=True)
class Err(Generic[_E]):
    error: _E


type Result[T, E] = Ok[T] | Err[E]
";

#[cfg(test)]
mod tests {
    use super::*;

    /// Scaffold a minimal project under `dir` and return the src and build paths.
    fn scaffold(
        dir: &std::path::Path,
        src_content: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let src_dir = dir.join("src");
        let out_dir = dir.join("build");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(
            dir.join("typhon.toml"),
            "[project]\nname = \"test\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
             [python]\ntarget = \"3.13\"\n[emit]\nformat = false\n[strictness]\n[env]\n",
        )
        .unwrap();
        std::fs::write(src_dir.join("main.ty"), src_content).unwrap();
        (src_dir, out_dir)
    }

    #[test]
    fn build_produces_py_file_from_simple_source() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, out_dir) = scaffold(tmp.path(), "val greeting: str = \"hello\"\n");
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
        })
        .unwrap();
        assert!(
            out_dir.join("main.py").exists(),
            "main.py should be emitted"
        );
    }

    #[test]
    fn build_out_flag_overrides_config_out_dir() {
        let tmp = tempfile::tempdir().unwrap();
        scaffold(tmp.path(), "val x: int = 42\n");
        let custom_out = tmp.path().join("custom_out");
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: Some(custom_out.clone()),
            no_format: true,
        })
        .unwrap();
        assert!(
            custom_out.join("main.py").exists(),
            "output should go to custom_out/"
        );
    }

    #[test]
    fn build_fails_on_type_error() {
        let tmp = tempfile::tempdir().unwrap();
        scaffold(tmp.path(), "val x: int = \"wrong type\"\n");
        let result = run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
        });
        assert!(result.is_err(), "build should fail on type mismatch");
    }

    #[test]
    fn build_emits_typhon_runtime_when_result_used() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, out_dir) = scaffold(tmp.path(), "def f() -> Ok[int]:\n    return Ok(1)\n");
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
        })
        .unwrap();
        assert!(
            out_dir.join("typhon_runtime.py").exists(),
            "typhon_runtime.py should be emitted when Ok/Err are used"
        );
    }
}
