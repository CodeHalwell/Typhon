//! Shared helpers used by multiple `tyc` subcommands.

use std::path::{Path, PathBuf};

use miette::{miette, Result};

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
