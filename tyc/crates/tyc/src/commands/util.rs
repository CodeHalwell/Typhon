//! Shared helpers used by multiple `tyc` subcommands.

use std::path::{Path, PathBuf};

use miette::{miette, Result};
use tyc_diagnostics::{Diagnostics, TycError};

use crate::config::TyphonConfig;

/// Re-classify warnings as errors according to the `[strictness]` config section.
///
/// Currently handles `unused-import`:
/// - `"warn"` (default): `UnusedImport` diagnostics remain warnings.
/// - `"error"`: `UnusedImport` diagnostics are promoted to errors.
///
/// All other warnings are passed through unchanged.
pub fn apply_strictness(diags: Diagnostics, config: &TyphonConfig) -> Diagnostics {
    let promote_unused_import = config.strictness.unused_import == "error";
    if !promote_unused_import {
        return diags;
    }
    let mut new_diags = Diagnostics::new();
    for err in diags.errors().iter().cloned() {
        new_diags.push_error(err);
    }
    for warn in diags.warnings().iter().cloned() {
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
    collect_into(root, &mut acc)?;
    Ok(acc)
}

fn collect_into(root: &Path, acc: &mut Vec<PathBuf>) -> Result<()> {
    if root.is_file() {
        if root.extension().map(|e| e == "ty").unwrap_or(false) {
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
            collect_into(&path, acc)?;
        }
    }
    Ok(())
}
