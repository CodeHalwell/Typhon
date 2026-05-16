//! `tyc build` — full compilation pipeline.
//!
//! Runs: pre-process → parse → type-check → desugar → emit.
//! Writes `.py` files into the output directory, mirroring the source tree.

use std::path::{Path, PathBuf};

use clap::Args;
use miette::{miette, Result};
use rustpython_parser::{parse, Mode};

use tyc_db::{check_file, TycDatabase};
use tyc_desugar::desugar_module;
use tyc_emit::emit;
use tyc_syntax::preprocess::preprocess;

use crate::config::TyphonConfig;

/// Arguments for `tyc build`.
#[derive(Args, Debug)]
pub struct BuildArgs {
    /// Project directory (defaults to current directory).
    #[arg(value_name = "PATH", default_value = ".")]
    pub path: PathBuf,

    /// Write output to this directory instead of the configured `out` dir.
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

    // Load typhon.toml (or use defaults if none found).
    let config = match TyphonConfig::load(&project_root) {
        Ok(Some((_, cfg))) => cfg,
        Ok(None) => {
            eprintln!("warning: no typhon.toml found; using defaults");
            TyphonConfig::default()
        }
        Err(e) => return Err(miette!("{e}")),
    };

    let src_dir = project_root.join(&config.project.src);
    let out_dir = args.out.unwrap_or_else(|| project_root.join(&config.project.out));

    if !src_dir.exists() {
        return Err(miette!(
            "source directory '{}' does not exist",
            src_dir.display()
        ));
    }

    let mut ty_files: Vec<PathBuf> = Vec::new();
    collect_ty_files(&src_dir, &mut ty_files)?;

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

    for (path, source) in &sources {
        let prep = preprocess(source);

        let module = parse(&prep.python_source, Mode::Module, &path.display().to_string())
            .map_err(|e| miette!("parse error in '{}': {e}", path.display()))?;

        let desugared = desugar_module(&module);
        let python_src = emit(&desugared);

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

    println!("built {} file(s) → '{}'", emitted, out_dir.display());
    Ok(())
}

fn collect_ty_files(root: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if root.is_file() {
        if root.extension().map(|e| e == "ty").unwrap_or(false) {
            out.push(root.to_path_buf());
        }
        return Ok(());
    }
    if root.is_dir() {
        let entries = std::fs::read_dir(root)
            .map_err(|e| miette!("cannot read directory '{}': {e}", root.display()))?;
        let mut paths = Vec::new();
        for entry in entries {
            paths.push(
                entry
                    .map_err(|e| miette!("cannot read entry in '{}': {e}", root.display()))?
                    .path(),
            );
        }
        paths.sort();
        for path in paths {
            collect_ty_files(&path, out)?;
        }
    }
    Ok(())
}
