//! `tyc build` — full compilation pipeline.
//!
//! Runs: pre-process → parse → type-check → desugar → emit.
//! Writes `.py` files into the output directory, mirroring the source tree.

use std::path::PathBuf;

use clap::Args;
use miette::{miette, Result};
use rustpython_parser::{parse, Mode};

use tyc_db::{check_file, TycDatabase};
use tyc_desugar::desugar_module;
use tyc_emit::emit;
use tyc_format::format_source;
use tyc_syntax::preprocess::preprocess;

use crate::commands::util::collect_ty_files;
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
    let project_root = args.path.canonicalize().map_err(|e| {
        miette!("cannot resolve path '{}': {}", args.path.display(), e)
    })?;

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
    let mut error_count = 0usize;

    for (path, source) in &sources {
        let diags = check_file(&mut db, path.display().to_string(), source.clone());
        if diags.has_errors() {
            for err in diags.errors() {
                eprintln!("{:?}", miette::Report::new_boxed(Box::new(err.clone())));
            }
            error_count += diags.error_count();
        }
    }

    if error_count > 0 {
        return Err(miette!(
            "{error_count} error(s) — fix type errors before building"
        ));
    }

    // Phase 2: desugar and emit using the already-loaded source text.
    let mut emitted = 0usize;
    let mut needs_runtime = false;

    for (path, source) in &sources {
        let prep = preprocess(source);

        let module = parse(&prep.python_source, Mode::Module, &path.display().to_string())
            .map_err(|e| miette!("parse error in '{}': {e}", path.display()))?;

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

        let rel = path.strip_prefix(&src_dir).map_err(|_| {
            miette!("'{}' is outside the source directory", path.display())
        })?;
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
