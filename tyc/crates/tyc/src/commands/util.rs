//! Shared helpers used by multiple `tyc` subcommands.

use std::path::{Path, PathBuf};

use miette::{miette, Result};
use tyc_diagnostics::{Diagnostics, TycError};

use crate::config::TyphonConfig;

/// Re-classify warnings according to the `[strictness]` config section.
///
/// Currently handles:
/// - `unused-import`:
///   - `"warn"`: `UnusedImport` diagnostics remain warnings.
///   - `"error"` (default): `UnusedImport` diagnostics are promoted to errors.
/// - `methods-in-class-body`:
///   - `"warn"` (default): `MethodInClassBody` stays a warning.
///   - `"error"`: `MethodInClassBody` is promoted to an error so CI breaks
///     on Rule-4 violations (methods belong in `impl Name:`, not the
///     class body).
///   - `"off"`: `MethodInClassBody` is dropped entirely — useful for
///     codebases still mid-migration to the `impl`-only form.
///
/// All other warnings are passed through unchanged. The function consumes the
/// input `Diagnostics` to avoid cloning individual diagnostics.
pub fn apply_strictness(diags: Diagnostics, config: &TyphonConfig) -> Diagnostics {
    let promote_unused_import = config.strictness.unused_import == "error";
    let methods_in_class_body = config.strictness.methods_in_class_body.as_str();
    if !promote_unused_import
        && (methods_in_class_body == "warn" || methods_in_class_body.is_empty())
    {
        return diags;
    }
    let (errors, warnings) = diags.into_parts();
    let mut new_diags = Diagnostics::new();
    for err in errors {
        new_diags.push_error(err);
    }
    for warn in warnings {
        if promote_unused_import && matches!(warn, TycError::UnusedImport { .. }) {
            new_diags.push_error(warn);
        } else if matches!(warn, TycError::MethodInClassBody { .. }) {
            match methods_in_class_body {
                "off" => {} // drop the diagnostic entirely
                "error" => new_diags.push_error(warn),
                _ => new_diags.push_warning(warn),
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TyphonConfig;

    fn config_with_methods_in_class_body(severity: &str) -> TyphonConfig {
        let mut c = TyphonConfig::default();
        c.strictness.methods_in_class_body = severity.into();
        c
    }

    fn method_in_class_body_warning() -> TycError {
        TycError::method_in_class_body("Button", "draw", "<test>", "x", 0, 1)
    }

    #[test]
    fn methods_in_class_body_warn_keeps_as_warning() {
        let mut diags = Diagnostics::new();
        diags.push_warning(method_in_class_body_warning());
        let out = apply_strictness(diags, &config_with_methods_in_class_body("warn"));
        assert_eq!(out.errors().len(), 0);
        assert_eq!(out.warning_count(), 1);
    }

    #[test]
    fn methods_in_class_body_error_promotes_to_error() {
        let mut diags = Diagnostics::new();
        diags.push_warning(method_in_class_body_warning());
        let out = apply_strictness(diags, &config_with_methods_in_class_body("error"));
        assert_eq!(out.errors().len(), 1);
        assert_eq!(out.warning_count(), 0);
    }

    #[test]
    fn methods_in_class_body_off_drops_the_diagnostic() {
        let mut diags = Diagnostics::new();
        diags.push_warning(method_in_class_body_warning());
        let out = apply_strictness(diags, &config_with_methods_in_class_body("off"));
        assert_eq!(out.errors().len(), 0);
        assert_eq!(out.warning_count(), 0);
    }

    #[test]
    fn methods_in_class_body_does_not_affect_other_warnings() {
        // Promoting MethodInClassBody to error must not also promote other
        // warnings (e.g. UnusedImport, which has its own knob).
        let mut diags = Diagnostics::new();
        diags.push_warning(method_in_class_body_warning());
        diags.push_warning(TycError::unused_import("os", "<test>", "import os", 0, 9));
        let mut config = TyphonConfig::default();
        config.strictness.unused_import = "warn".into();
        config.strictness.methods_in_class_body = "error".into();
        let out = apply_strictness(diags, &config);
        // MethodInClassBody promoted, UnusedImport stays a warning.
        assert_eq!(out.errors().len(), 1);
        assert_eq!(out.warning_count(), 1);
        assert!(matches!(
            out.errors()[0],
            TycError::MethodInClassBody { .. }
        ));
    }
}
