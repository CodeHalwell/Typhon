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

#[cfg(test)]
mod tests {
    use super::*;

    fn write_ty(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn fmt_passes_on_already_formatted_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "a.ty", "let x: int = 1\n");
        let args = FmtArgs {
            paths: vec![tmp.path().to_path_buf()],
            check: false,
        };
        run(args).unwrap();
    }

    #[test]
    fn fmt_normalises_tab_indentation() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_ty(tmp.path(), "b.ty", "def f():\n\tpass\n");
        let args = FmtArgs {
            paths: vec![path.clone()],
            check: false,
        };
        run(args).unwrap();
        let result = std::fs::read_to_string(&path).unwrap();
        assert!(
            result.contains("    pass"),
            "tabs should be replaced by spaces"
        );
    }

    #[test]
    fn fmt_check_mode_detects_unformatted_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "c.ty", "def f():\n\tpass\n");
        let args = FmtArgs {
            paths: vec![tmp.path().to_path_buf()],
            check: true,
        };
        // check mode should report an error because the file would be changed
        assert!(run(args).is_err());
    }

    #[test]
    fn fmt_check_mode_passes_on_already_formatted_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "d.ty", "let x: int = 1\n");
        let args = FmtArgs {
            paths: vec![tmp.path().to_path_buf()],
            check: true,
        };
        run(args).unwrap();
    }
}
