//! `tyc build` — full compilation pipeline: parse, check, emit, write.
//!
//! Reads all `.ty` source files in the project's `src` directory, runs the
//! Phase-1 check pipeline across all of them, and — if clean — emits a
//! corresponding `.py` file to the configured `out` directory, mirroring the
//! source tree structure.

use std::path::PathBuf;

use clap::Args;
use miette::{miette, Result};
use rustpython_parser::{parse, Mode};

use tyc_db::{check_file, TycDatabase};
use tyc_diagnostics::{Diagnostics, TycError};
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
    let project_root = args
        .path
        .canonicalize()
        .unwrap_or_else(|_| args.path.clone());

    // Determine source and output directories from typhon.toml (if present).
    let (src_dir, configured_out) = match TyphonConfig::load(&project_root)
        .map_err(|e| miette!("{}", e))?
    {
        Some((config_path, cfg)) => {
            let root = config_path
                .parent()
                .unwrap_or(&project_root)
                .to_path_buf();
            (root.join(&cfg.project.src), root.join(&cfg.project.out))
        }
        None => (project_root.clone(), project_root.join("build")),
    };

    let out_dir = args.out.as_ref().cloned().unwrap_or(configured_out);

    // Collect all .ty source files.
    let mut ty_files: Vec<PathBuf> = Vec::new();
    collect_ty_files(&src_dir, &mut ty_files)?;

    if ty_files.is_empty() {
        println!("no .ty files found in {}", src_dir.display());
        return Ok(());
    }

    // Phase 1: check every file and accumulate diagnostics before writing
    // any output (all-or-nothing semantics: one error stops the build).
    let mut all_diags = Diagnostics::new();
    let mut db = TycDatabase::new();
    let mut file_sources: Vec<(PathBuf, String)> = Vec::new();

    for path in &ty_files {
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                all_diags.push_error(TycError::io(path.display().to_string(), &e));
                continue;
            }
        };
        let diags = check_file(&mut db, path.display().to_string(), source.clone());
        all_diags.extend(diags);
        file_sources.push((path.clone(), source));
    }

    if all_diags.has_errors() {
        for err in all_diags.errors() {
            eprintln!("{:?}", miette::Report::new_boxed(Box::new(err.clone())));
        }
        return Err(miette!(
            "{} error(s) in {} file(s) — no output written",
            all_diags.error_count(),
            ty_files.len()
        ));
    }

    // Phase 2: emit Python for every clean file.
    std::fs::create_dir_all(&out_dir).map_err(|e| {
        miette!(
            "cannot create output directory {}: {}",
            out_dir.display(),
            e
        )
    })?;

    let mut emitted = 0usize;
    for (path, source) in &file_sources {
        let path_str = path.display().to_string();

        // Preprocess strips `val`/`var` and rewrites `T?` → `T | None` so
        // the underlying rustpython-parser can handle the source.
        let prep = preprocess(source);

        let module = parse(&prep.python_source, Mode::Module, &path_str)
            .map_err(|e| miette!("internal parse error in {}: {}", path_str, e))?;

        let py_source = emit(&module);

        // Mirror the source tree under the output directory.
        let rel = path.strip_prefix(&src_dir).unwrap_or(path.as_path());
        let out_path = out_dir.join(rel).with_extension("py");

        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| miette!("cannot create {}: {}", parent.display(), e))?;
        }

        std::fs::write(&out_path, &py_source)
            .map_err(|e| miette!("cannot write {}: {}", out_path.display(), e))?;

        println!("  {} → {}", path_str, out_path.display());
        emitted += 1;
    }

    println!("built {} file(s) → {}", emitted, out_dir.display());
    Ok(())
}

fn collect_ty_files(root: &PathBuf, out: &mut Vec<PathBuf>) -> Result<()> {
    if root.is_file() {
        if root.extension().map(|e| e == "ty").unwrap_or(false) {
            out.push(root.clone());
        }
        return Ok(());
    }
    if root.is_dir() {
        let entries = std::fs::read_dir(root)
            .map_err(|e| miette!("cannot read directory {}: {}", root.display(), e))?;
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        paths.sort();
        for path in paths {
            collect_ty_files(&path, out)?;
        }
    }
    Ok(())
}

