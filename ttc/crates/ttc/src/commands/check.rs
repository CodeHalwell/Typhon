//! `ttc check` — parse and type-check without emitting code.

use std::path::PathBuf;

use clap::Args;
use miette::{miette, Result};

use ttc_diagnostics::{Diagnostics, TtcError};
use ttc_syntax::{parser::parse_module, preprocess::preprocess};

/// Arguments for `ttc check`.
#[derive(Args, Debug)]
pub struct CheckArgs {
    /// `.tt` files or directories to check.
    #[arg(value_name = "PATH", default_value = ".")]
    pub paths: Vec<PathBuf>,
}

pub fn run(args: CheckArgs) -> Result<()> {
    let mut diags = Diagnostics::new();
    let mut file_count = 0usize;

    for root in &args.paths {
        collect_tt_files(root, &mut |path| {
            file_count += 1;

            let source = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => {
                    diags.push_error(TtcError::io(path.display().to_string(), &e));
                    return;
                }
            };

            let prep = preprocess(&source);

            // The parse error offset is relative to `prep.python_source`, so
            // use that as the diagnostic source text to keep spans aligned.
            if let Err(e) = parse_module(&prep.python_source, &path.display().to_string()) {
                diags.push_error(TtcError::parse(
                    path.display().to_string(),
                    &prep.python_source,
                    e.to_string(),
                    usize::from(e.offset),
                ));
            }
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

fn collect_tt_files<F>(root: &PathBuf, f: &mut F) -> Result<()>
where
    F: FnMut(&PathBuf),
{
    if root.is_file() {
        if root.extension().map(|e| e == "tt").unwrap_or(false) {
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
            collect_tt_files(&path, f)?;
        }
        return Ok(());
    }
    Err(miette!(
        "path does not exist: {}",
        root.display()
    ))
}
