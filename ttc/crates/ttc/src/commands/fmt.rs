//! `ttc fmt` — format `.tt` source files.

use std::path::PathBuf;

use clap::Args;
use miette::{miette, Result};

use ttc_format::format_file;

/// Arguments for `ttc fmt`.
#[derive(Args, Debug)]
pub struct FmtArgs {
    /// `.tt` files or directories to format.
    ///
    /// When a directory is given, all `.tt` files within it are formatted
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
        collect_tt_files(root, &mut |path| {
            total += 1;

            if args.check {
                // In check mode, read the file and test whether it would change.
                let source = std::fs::read_to_string(path)
                    .map_err(|e| ttc_diagnostics::TtcError::io(path.display().to_string(), &e))?;
                let result =
                    ttc_format::format_source(&source, &path.display().to_string())?;
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
        eprintln!("ttc fmt: no .tt files found");
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

/// Recursively collect `.tt` files under `root` and call `f` for each.
fn collect_tt_files<F>(root: &PathBuf, f: &mut F) -> Result<()>
where
    F: FnMut(&PathBuf) -> std::result::Result<(), ttc_diagnostics::TtcError>,
{
    if root.is_file() {
        if root.extension().map(|e| e == "tt").unwrap_or(false) {
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
            collect_tt_files(&path, f)?;
        }
    }

    Ok(())
}
