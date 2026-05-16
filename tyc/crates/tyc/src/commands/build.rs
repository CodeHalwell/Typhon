//! `tyc build` — full compilation pipeline: parse, check, desugar, emit.
//!
//! Reads `.ty` source files from the configured `src/` directory, runs the
//! Phase-1 type-checking pipeline, and emits idiomatic `.py` output to the
//! configured `out/` directory.  Class definitions are emitted as
//! `@dataclass(slots=True)` by default.

use std::path::{Path, PathBuf};

use clap::Args;
use miette::{miette, Result};

use tyc_db::{build_file, TycDatabase};
use tyc_desugar::ClassDefault;

use crate::config::TyphonConfig;

/// Arguments for `tyc build`.
#[derive(Args, Debug)]
pub struct BuildArgs {
    /// Project directory or a single `.ty` file (defaults to current directory).
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
    let project_dir = args
        .path
        .canonicalize()
        .map_err(|e| miette!("cannot resolve path {}: {}", args.path.display(), e))?;

    // Determine source directory, output directory, and class emission strategy
    // from typhon.toml (if present), falling back to sensible defaults.
    let (src_dir, out_dir, class_default) =
        match TyphonConfig::load(&project_dir).map_err(|e| miette!("{}", e))? {
            Some((config_path, config)) => {
                let root = config_path
                    .parent()
                    .unwrap_or(&project_dir)
                    .to_path_buf();
                let src = root.join(&config.project.src);
                let out = args.out.clone().unwrap_or_else(|| root.join(&config.project.out));
                let cd = ClassDefault::from_str(&config.emit.class_default);
                (src, out, cd)
            }
            None => {
                // No typhon.toml: use args.path as the source root.
                let src = if project_dir.is_file() {
                    project_dir
                        .parent()
                        .unwrap_or(&project_dir)
                        .to_path_buf()
                } else {
                    project_dir.clone()
                };
                let out = args.out.clone().unwrap_or_else(|| {
                    project_dir
                        .parent()
                        .unwrap_or(&project_dir)
                        .join("build")
                });
                (src, out, ClassDefault::default())
            }
        };

    // Collect .ty files.
    let mut ty_files: Vec<PathBuf> = Vec::new();
    if project_dir.is_file()
        && project_dir.extension().map(|e| e == "ty").unwrap_or(false)
    {
        ty_files.push(project_dir.clone());
    } else {
        collect_ty_files(&src_dir, &mut |p| ty_files.push(p.clone()))?;
    }

    if ty_files.is_empty() {
        return Err(miette!(
            "no .ty source files found in {}",
            src_dir.display()
        ));
    }

    std::fs::create_dir_all(&out_dir)
        .map_err(|e| miette!("cannot create output directory {}: {}", out_dir.display(), e))?;

    let mut db = TycDatabase::new();
    let mut error_count = 0usize;
    let mut emitted_count = 0usize;

    for ty_path in &ty_files {
        let source = match std::fs::read_to_string(ty_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "error: cannot read {}: {}",
                    ty_path.display(),
                    e
                );
                error_count += 1;
                continue;
            }
        };

        let output = build_file(
            &mut db,
            ty_path.display().to_string(),
            source,
            class_default,
        );

        if output.diagnostics.has_errors() {
            for err in output.diagnostics.errors() {
                eprintln!(
                    "{:?}",
                    miette::Report::new_boxed(Box::new(err.clone()))
                );
            }
            error_count += output.diagnostics.error_count();
            continue;
        }

        if let Some(python) = output.python_source {
            let out_path = output_path(ty_path, &src_dir, &out_dir);
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    miette!("cannot create directory {}: {}", parent.display(), e)
                })?;
            }
            std::fs::write(&out_path, &python)
                .map_err(|e| miette!("cannot write {}: {}", out_path.display(), e))?;
            println!("  {} → {}", ty_path.display(), out_path.display());
            emitted_count += 1;
        }
    }

    if error_count > 0 {
        return Err(miette!("{} error(s) — build failed", error_count));
    }

    println!(
        "built {} file(s) → {}",
        emitted_count,
        out_dir.display()
    );
    Ok(())
}

/// Compute the output `.py` path for a given `.ty` source file.
///
/// Preserves the directory structure relative to `src_dir`.  If the source
/// is not under `src_dir` (e.g. a bare file passed on the command line),
/// places the output directly in `out_dir`.
fn output_path(ty_path: &Path, src_dir: &Path, out_dir: &Path) -> PathBuf {
    let rel = ty_path
        .strip_prefix(src_dir)
        .map(|r| r.to_path_buf())
        .unwrap_or_else(|_| {
            PathBuf::from(ty_path.file_name().unwrap_or_default())
        });
    out_dir.join(rel).with_extension("py")
}

fn collect_ty_files<F>(root: &Path, f: &mut F) -> Result<()>
where
    F: FnMut(&PathBuf),
{
    if root.is_file() {
        if root.extension().map(|e| e == "ty").unwrap_or(false) {
            f(&root.to_path_buf());
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
            collect_ty_files(&path, f)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tyc_db::{build_file, TycDatabase};

    #[test]
    fn builds_simple_ty_file() {
        let mut db = TycDatabase::new();
        let src = "val x: int = 1\n";
        let output =
            build_file(&mut db, "<test>".into(), src.into(), ClassDefault::Dataclass);
        assert!(!output.diagnostics.has_errors(), "{:?}", output.diagnostics.errors());
        let py = output.python_source.expect("expected python source");
        assert!(py.contains("# generated by tyc"), "missing header: {py}");
        assert!(py.contains("x: int = 1"), "missing binding: {py}");
    }

    #[test]
    fn injects_dataclass_for_class_def() {
        let mut db = TycDatabase::new();
        let src = "class User:\n    id: int\n    name: str\n";
        let output =
            build_file(&mut db, "<test>".into(), src.into(), ClassDefault::Dataclass);
        assert!(!output.diagnostics.has_errors(), "{:?}", output.diagnostics.errors());
        let py = output.python_source.unwrap();
        assert!(
            py.contains("from dataclasses import dataclass"),
            "missing import: {py}"
        );
        assert!(py.contains("@dataclass(slots=True)"), "missing decorator: {py}");
    }

    #[test]
    fn skips_dataclass_for_class_default_none() {
        let mut db = TycDatabase::new();
        let src = "class User:\n    id: int\n";
        let output =
            build_file(&mut db, "<test>".into(), src.into(), ClassDefault::None);
        assert!(!output.diagnostics.has_errors());
        let py = output.python_source.unwrap();
        assert!(!py.contains("dataclass"), "unexpected dataclass: {py}");
    }

    #[test]
    fn build_fails_on_type_error() {
        let mut db = TycDatabase::new();
        let src = "val x: int = \"not an int\"\n";
        let output =
            build_file(&mut db, "<test>".into(), src.into(), ClassDefault::Dataclass);
        assert!(output.diagnostics.has_errors());
        assert!(output.python_source.is_none());
    }
}
