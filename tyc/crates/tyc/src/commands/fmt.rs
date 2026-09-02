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
    // A file the formatter cannot parse does NOT abort the walk. One
    // work-in-progress file used to stop `tyc fmt src/` dead, leaving the
    // tree half-formatted with no record of which files were never
    // visited, and made `tyc fmt --check` in CI a one-error-per-run loop.
    // Every other formatter in the ecosystem reports each failure and
    // carries on, exiting non-zero at the end; so does this one.
    let mut failed = 0usize;

    for root in &args.paths {
        for path in collect_ty_files(root)? {
            total += 1;

            let outcome = if args.check {
                std::fs::read_to_string(&path)
                    .map_err(|e| {
                        Report::new(tyc_diagnostics::TycError::io(
                            path.to_string_lossy().into_owned(),
                            &e,
                        ))
                    })
                    .and_then(|source| {
                        tyc_format::format_source(&source, &path.to_string_lossy())
                            .map(|result| result.changed)
                            .map_err(Report::new)
                    })
            } else {
                format_file(&path).map_err(Report::new)
            };

            match outcome {
                Ok(true) => {
                    changed += 1;
                    if args.check {
                        eprintln!("would reformat: {}", path.display());
                    } else {
                        println!("reformatted: {}", path.display());
                    }
                }
                Ok(false) => {}
                Err(report) => {
                    failed += 1;
                    eprintln!("{:?}", report);
                }
            }
        }
    }

    if failed > 0 {
        return Err(miette!(
            "{} file{} could not be formatted",
            failed,
            if failed == 1 { "" } else { "s" }
        ));
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

    #[test]
    fn fmt_check_exits_nonzero_on_differences() {
        // `--check` against an unformatted file must surface an Err so the
        // process exits non-zero (CI gating uses this).
        let tmp = tempfile::tempdir().unwrap();
        // Trailing whitespace on the line is normalised away by the
        // in-process pipeline, regardless of whether ruff is installed.
        write_ty(tmp.path(), "e.ty", "let x: int = 1   \n");
        let args = FmtArgs {
            paths: vec![tmp.path().to_path_buf()],
            check: true,
        };
        assert!(run(args).is_err());
    }

    #[test]
    fn fmt_keeps_going_past_a_file_it_cannot_parse() {
        // A single unparseable file must not abort the walk: every other
        // file in the tree still gets formatted, and the command still
        // exits non-zero so the failure is not silent.
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "a_broken.ty", "def f(:\n");
        let good = write_ty(tmp.path(), "z_good.ty", "def f():\n\tpass\n");
        let args = FmtArgs {
            paths: vec![tmp.path().to_path_buf()],
            check: false,
        };
        assert!(
            run(args).is_err(),
            "an unformattable file must fail the run"
        );
        // `a_broken.ty` sorts first, so the old `?` bailed out before ever
        // reaching this file.
        let formatted = std::fs::read_to_string(&good).unwrap();
        assert!(
            formatted.contains("    pass"),
            "the file after the broken one must still be formatted, got {formatted:?}"
        );
    }

    #[test]
    fn fmt_check_reports_every_unparseable_file() {
        // `--check` in CI should surface all the failures in one run
        // rather than one per invocation.
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "a.ty", "def f(:\n");
        write_ty(tmp.path(), "b.ty", "class C(:\n");
        let args = FmtArgs {
            paths: vec![tmp.path().to_path_buf()],
            check: true,
        };
        let err = run(args).unwrap_err();
        assert!(
            err.to_string().contains("2 files could not be formatted"),
            "expected both failures to be counted, got {err}"
        );
    }

    #[test]
    fn fmt_check_returns_ok_for_clean_input() {
        // Already-formatted source must round-trip cleanly under --check
        // (no Err, no diff messages, exit 0).
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "f.ty", "let x: int = 1\n");
        let args = FmtArgs {
            paths: vec![tmp.path().to_path_buf()],
            check: true,
        };
        assert!(run(args).is_ok());
    }
}
