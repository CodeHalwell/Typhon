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

use crate::commands::util::{apply_strictness, collect_dty_files, collect_ty_files};
use crate::config::TyphonConfig;

/// Arguments for `tyc check`.
#[derive(Args, Debug)]
pub struct CheckArgs {
    /// `.ty` files or directories to check.
    #[arg(value_name = "PATH", default_value = ".")]
    pub paths: Vec<PathBuf>,

    /// Validate `.dty` stub files against the runtime modules they describe.
    ///
    /// In v1 this flag only validates that every `.dty` file parses, resolves,
    /// and type-checks cleanly — the full stubtest-style runtime comparison
    /// (which would import the implementation module and diff its symbols) is
    /// deferred. The flag is recognised today so CI configurations are
    /// forward-compatible.
    #[arg(long)]
    pub stubs: bool,
}

pub fn run(args: CheckArgs) -> Result<()> {
    // Load strictness config from `typhon.toml`, anchoring the search to the
    // first checked path (or CWD when none is provided) so that
    // `tyc check path/to/project` uses that project's config, not the caller's.
    let config_start = args
        .paths
        .first()
        .map(|p| {
            if p.is_file() {
                p.parent()
                    .map(|d| d.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."))
            } else {
                p.clone()
            }
        })
        .unwrap_or_else(|| PathBuf::from("."));
    let config = match TyphonConfig::load(&config_start) {
        Ok(Some((_, cfg))) => cfg,
        Ok(None) => TyphonConfig::default(),
        Err(e) => return Err(miette!("{e}")),
    };

    let mut diags = Diagnostics::new();
    let mut file_count = 0usize;
    let mut db = TycDatabase::new();

    for root in &args.paths {
        for path in collect_ty_files(root)? {
            file_count += 1;

            let source = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    diags.push_error(TycError::io(path.display().to_string(), &e));
                    continue;
                }
            };

            let file_diags = check_file(&mut db, path.display().to_string(), source);
            diags.extend(file_diags);
        }

        // `--stubs`: also parse + type-check every `.dty` stub file under
        // the same root so a malformed stub is surfaced. The full runtime
        // diff against the implementation module is a future enhancement.
        if args.stubs {
            for path in collect_dty_files(root)? {
                file_count += 1;
                let source = match std::fs::read_to_string(&path) {
                    Ok(s) => s,
                    Err(e) => {
                        diags.push_error(TycError::io(path.display().to_string(), &e));
                        continue;
                    }
                };
                let file_diags = check_file(&mut db, path.display().to_string(), source);
                diags.extend(file_diags);
            }
        }
    }

    // Apply strictness rules (e.g. promote unused-import warnings to errors).
    let diags = apply_strictness(diags, &config);

    // Emit warnings regardless of whether there are errors.
    for warn in diags.warnings() {
        eprintln!("{:?}", miette::Report::new_boxed(Box::new(warn.clone())));
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

    if diags.warning_count() > 0 {
        println!(
            "checked {} file(s) — {} warning(s)",
            file_count,
            diags.warning_count()
        );
    } else {
        println!("checked {} file(s) — no errors", file_count);
    }
    Ok(())
}
