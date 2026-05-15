//! `ttc check` — parse and type-check without emitting code.

use std::path::PathBuf;

use clap::Args;
use miette::{miette, Result};

use ttc_diagnostics::TtcError;
use ttc_syntax::{parser::parse_module, preprocess::preprocess};

/// Arguments for `ttc check`.
#[derive(Args, Debug)]
pub struct CheckArgs {
    /// `.tt` files or directories to check.
    #[arg(value_name = "PATH", default_value = ".")]
    pub paths: Vec<PathBuf>,
}

pub fn run(args: CheckArgs) -> Result<()> {
    let mut error_count = 0usize;
    let mut file_count = 0usize;

    for root in &args.paths {
        collect_tt_files(root, &mut |path| {
            file_count += 1;
            let source = std::fs::read_to_string(path)
                .map_err(|e| TtcError::io(path.display().to_string(), &e))?;
            let prep = preprocess(&source);
            parse_module(&prep.python_source, &path.display().to_string())
                .map(|_| ())
                .map_err(|e| {
                    TtcError::parse(
                        path.display().to_string(),
                        &source,
                        e.to_string(),
                        usize::from(e.offset),
                    )
                })
        })?;
    }

    if error_count > 0 {
        Err(miette!("{} error(s) found", error_count))
    } else {
        println!("checked {} file(s) — no errors", file_count);
        Ok(())
    }
}

fn collect_tt_files<F>(root: &PathBuf, f: &mut F) -> Result<()>
where
    F: FnMut(&PathBuf) -> std::result::Result<(), TtcError>,
{
    if root.is_file() {
        if root.extension().map(|e| e == "tt").unwrap_or(false) {
            f(root).map_err(|e| miette!("{}", e))?;
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
    }
    Ok(())
}
