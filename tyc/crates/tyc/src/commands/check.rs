//! `tyc check` — parse, resolve, and type-check without emitting code.
//!
//! Runs the full Phase-1 pipeline: pre-process → parse → resolve
//! (scopes + `val`/`var` enforcement + unknown-name diagnostics) → type
//! check (nominal types, non-null narrowing, argument-count and
//! argument-type checks). Diagnostics are rendered via miette.

use std::path::PathBuf;

use clap::Args;
use miette::{miette, Result};

use tyc_db::{check_file, TycDatabase};
use tyc_diagnostics::{Diagnostics, TycError};

/// Arguments for `tyc check`.
#[derive(Args, Debug)]
pub struct CheckArgs {
    /// `.ty` files or directories to check.
    #[arg(value_name = "PATH", default_value = ".")]
    pub paths: Vec<PathBuf>,
}

pub fn run(args: CheckArgs) -> Result<()> {
    let mut diags = Diagnostics::new();
    let mut file_count = 0usize;
    let mut db = TycDatabase::new();

    for root in &args.paths {
        collect_ty_files(root, &mut |path| {
            file_count += 1;

            let source = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => {
                    diags.push_error(TycError::io(path.display().to_string(), &e));
                    return;
                }
            };

            let file_diags = check_file(&mut db, path.display().to_string(), source);
            diags.extend(file_diags);
        })?;
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

    println!("checked {} file(s) — no errors", file_count);
    Ok(())
}

fn collect_ty_files<F>(root: &PathBuf, f: &mut F) -> Result<()>
where
    F: FnMut(&PathBuf),
{
    if root.is_file() {
        if root.extension().map(|e| e == "ty").unwrap_or(false) {
            f(root);
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
