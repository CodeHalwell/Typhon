//! Shared helpers used by multiple `tyc` subcommands.

use std::path::{Path, PathBuf};

use miette::{miette, Result};
use tyc_diagnostics::{Diagnostics, TycError};

use crate::config::TyphonConfig;

/// Re-classify warnings as errors according to the `[strictness]` config section.
///
/// Currently handles `unused-import`:
/// - `"warn"`: `UnusedImport` diagnostics remain warnings.
/// - `"error"` (default): `UnusedImport` diagnostics are promoted to errors.
///
/// All other warnings are passed through unchanged.  The function consumes the
/// input `Diagnostics` to avoid cloning individual diagnostics.
pub fn apply_strictness(diags: Diagnostics, config: &TyphonConfig) -> Diagnostics {
    let promote_unused_import = config.strictness.unused_import == "error";
    if !promote_unused_import {
        return diags;
    }
    let (errors, warnings) = diags.into_parts();
    let mut new_diags = Diagnostics::new();
    for err in errors {
        new_diags.push_error(err);
    }
    for warn in warnings {
        if matches!(warn, TycError::UnusedImport { .. }) {
            new_diags.push_error(warn);
        } else {
            new_diags.push_warning(warn);
        }
    }
    new_diags
}

/// Recursively collect all `.ty` files under `root` in sorted order.
///
/// Returns an error if `root` does not exist or cannot be read. Non-`.ty`
/// files are silently skipped, but I/O problems and unreadable directory
/// entries are always reported so the caller never silently does nothing.
pub fn collect_ty_files(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.exists() {
        return Err(miette!("path does not exist: {}", root.display()));
    }
    let mut acc = Vec::new();
    collect_with_ext(root, "ty", &mut acc)?;
    Ok(acc)
}

/// Recursively collect all `.dty` stub files under `root` in sorted order.
/// Same contract as [`collect_ty_files`] but matches the `.dty` extension.
pub fn collect_dty_files(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.exists() {
        return Ok(Vec::new()); // optional — no stubs is fine
    }
    let mut acc = Vec::new();
    collect_with_ext(root, "dty", &mut acc)?;
    Ok(acc)
}

fn collect_with_ext(root: &Path, ext: &str, acc: &mut Vec<PathBuf>) -> Result<()> {
    if root.is_file() {
        if root.extension().map(|e| e == ext).unwrap_or(false) {
            acc.push(root.to_path_buf());
        }
        return Ok(());
    }
    if root.is_dir() {
        let entries = std::fs::read_dir(root)
            .map_err(|e| miette!("cannot read directory {}: {}", root.display(), e))?;
        let mut paths: Vec<PathBuf> = entries
            .map(|res| res.map(|e| e.path()))
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|e| miette!("cannot read directory entry in {}: {}", root.display(), e))?;
        paths.sort();
        for path in paths {
            collect_with_ext(&path, ext, acc)?;
        }
    }
    Ok(())
}
