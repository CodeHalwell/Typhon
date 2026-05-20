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

/// Map a project-relative source file path to its dotted Python
/// module name (e.g. `src/foo/bar.ty` with `src_root = "src"` →
/// `foo.bar`). Used by the cross-module shape registry and by the
/// unknown-import vetting pass so both keys match the resolver's
/// `ImportInfo::module` value.
pub fn path_to_dotted(path: &Path, src_root: &str) -> String {
    let components: Vec<String> = path
        .with_extension("")
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(os) => Some(os.to_string_lossy().to_string()),
            _ => None,
        })
        .collect();
    let src_idx = components.iter().rposition(|c| c == src_root);
    let tail: Vec<&str> = match src_idx {
        Some(i) => components[i + 1..].iter().map(|s| s.as_str()).collect(),
        None => components
            .last()
            .map(|s| vec![s.as_str()])
            .unwrap_or_default(),
    };
    let mut tail = tail;
    if tail.last().is_some_and(|s| *s == "__init__") {
        tail.pop();
    }
    tail.join(".")
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

/// Recursively collect all `.py` files under `root` in sorted order.
///
/// Mirrors [`collect_ty_files`] but matches the `.py` extension. Skips
/// `__pycache__/`, `tests/`, `.venv/`, and any hidden directory whose
/// name starts with `.` — these are never part of the user-authored
/// source tree that should be copied verbatim into the build output.
///
/// Returns `Ok(vec![])` (not an error) when `root` does not exist —
/// `.py` files alongside `.ty` files are optional.
pub fn collect_py_files(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut acc = Vec::new();
    collect_with_ext_filtered(root, "py", &mut acc)?;
    Ok(acc)
}

/// Variant of [`collect_with_ext`] that skips conventional non-source
/// directories: `__pycache__/`, `tests/`, `.venv/`, and any hidden
/// `.X` directory. Files are still matched by extension.
fn collect_with_ext_filtered(root: &Path, ext: &str, acc: &mut Vec<PathBuf>) -> Result<()> {
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
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name == "__pycache__"
                        || name == "tests"
                        || name == ".venv"
                        || name.starts_with('.')
                    {
                        continue;
                    }
                }
            }
            collect_with_ext_filtered(&path, ext, acc)?;
        }
    }
    Ok(())
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
    fn collect_py_files_picks_up_top_level_py_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.py"), "x = 1").unwrap();
        std::fs::write(tmp.path().join("b.py"), "x = 2").unwrap();
        std::fs::write(tmp.path().join("c.ty"), "let x: int = 3").unwrap();
        let files = collect_py_files(tmp.path()).unwrap();
        assert_eq!(files.len(), 2);
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"a.py".to_string()));
        assert!(names.contains(&"b.py".to_string()));
    }

    #[test]
    fn collect_py_files_recurses_into_subdirectories() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("pkg");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("nested.py"), "x = 1").unwrap();
        let files = collect_py_files(tmp.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("pkg/nested.py"));
    }

    #[test]
    fn collect_py_files_skips_excluded_directories() {
        let tmp = tempfile::tempdir().unwrap();
        for dirname in ["__pycache__", "tests", ".venv", ".hidden"] {
            let d = tmp.path().join(dirname);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("skip.py"), "x = 1").unwrap();
        }
        std::fs::write(tmp.path().join("keep.py"), "x = 1").unwrap();
        let files = collect_py_files(tmp.path()).unwrap();
        assert_eq!(files.len(), 1, "only keep.py should be returned: {files:?}");
        assert!(files[0].ends_with("keep.py"));
    }

    #[test]
    fn collect_py_files_missing_root_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope");
        let files = collect_py_files(&missing).unwrap();
        assert!(files.is_empty());
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
