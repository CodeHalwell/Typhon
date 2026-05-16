//! `tyc fmt` — format `.ty` source files.

use std::path::PathBuf;

use clap::Args;
use miette::{miette, Result};

use tyc_format::format_file;

/// Arguments for `tyc fmt`.
#[derive(Args, Debug)]
pub struct FmtArgs {
    /// `.ty` files or directories to format.
    ///
    /// When a directory is given, all `.ty` files within it are formatted
    /// recursively.  Defaults to the current directory (`.`).
    #[arg(value_name = "PATH", default_value = ".")]
    pub paths: Vec<PathBuf>,

    /// Check mode: exit with a non-zero code if any file would be changed,
    /// but do not write files.
    #[arg(long, short = 'c')]
    pub check: bool,
}

pub fn run(args: FmtArgs) -> Result<()> {
    let mut changed = 0usize;
    let mut total = 0usize;

    for root in &args.paths {
        collect_ty_files(root, &mut |path| {
            total += 1;

            if args.check {
                // In check mode, read the file and test whether it would change.
                let source = std::fs::read_to_string(path)
                    .map_err(|e| tyc_diagnostics::TycError::io(path.display().to_string(), &e))?;
                let result =
                    tyc_format::format_source(&source, &path.display().to_string())?;
                if result.changed {
                    changed += 1;
                    eprintln!("would reformat: {}", path.display());
                }
                Ok(())
            } else {
                let did_change = format_file(path)?;
                if did_change {
                    changed += 1;
                    println!("reformatted: {}", path.display());
                }
                Ok(())
            }
        })?;
    }

    if args.check && changed > 0 {
        return Err(miette!(
            "{} file{} would be reformatted",
            changed,
            if changed == 1 { "" } else { "s" }
        ));
    }

    if total == 0 {
        eprintln!("tyc fmt: no .ty files found");
    } else if !args.check {
        println!(
            "{} file{} reformatted, {} unchanged",
            changed,
            if changed == 1 { "" } else { "s" },
            total - changed,
        );
    }

    Ok(())
}

/// Recursively collect `.ty` files under `root` and call `f` for each.
fn collect_ty_files<F>(root: &PathBuf, f: &mut F) -> Result<()>
where
    F: FnMut(&PathBuf) -> std::result::Result<(), tyc_diagnostics::TycError>,
{
    if root.is_file() {
        if root.extension().map(|e| e == "ty").unwrap_or(false) {
            f(root).map_err(miette::Report::new)?;
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
