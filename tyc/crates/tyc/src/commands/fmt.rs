//! `tyc fmt` — format `.ty` source files.

use std::path::PathBuf;

use clap::Args;
use miette::{miette, Report, Result};

use tyc_format::format_file;

use crate::commands::util::collect_ty_files;

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
        for path in collect_ty_files(root)? {
            total += 1;

            if args.check {
                let source = std::fs::read_to_string(&path).map_err(|e| {
                    Report::new(tyc_diagnostics::TycError::io(
                        path.display().to_string(),
                        &e,
                    ))
                })?;
                let result = tyc_format::format_source(&source, &path.display().to_string())
                    .map_err(Report::new)?;
                if result.changed {
                    changed += 1;
                    eprintln!("would reformat: {}", path.display());
                }
            } else {
                let did_change = format_file(&path).map_err(Report::new)?;
                if did_change {
                    changed += 1;
                    println!("reformatted: {}", path.display());
                }
            }
        }
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
