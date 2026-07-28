//! Shared helpers used by multiple `tyc` subcommands.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use miette::{miette, Result};
use tyc_db::ModuleShapes;
use tyc_diagnostics::{Diagnostics, TycError};
use tyc_syntax::preprocess::preprocess;

use crate::config::TyphonConfig;

/// Re-classify warnings according to the `[strictness]` config section.
///
/// Currently handles:
/// - `unused-import`:
///   - `"warn"` (default): `UnusedImport` diagnostics remain warnings.
///     (FINDINGS v0.7.1 #41 — was promoted to error by default in pre-0.7.1
///     builds; almost every test required a cleanup pass, so the default
///     was relaxed.)
///   - `"error"`: `UnusedImport` diagnostics are promoted to errors so CI
///     breaks on stale imports.
/// - `methods-in-class-body`:
///   - `"warn"` (default): `MethodInClassBody` stays a warning.
///   - `"error"`: `MethodInClassBody` is promoted to an error so CI breaks
///     on Rule-4 violations (methods belong in `impl Name:`, not the
///     class body).
///   - `"off"`: `MethodInClassBody` is dropped entirely — useful for
///     codebases still mid-migration to the `impl`-only form.
/// - `stub-check`:
///   - `"error"` (default): `StubMismatch` diagnostics are promoted to errors.
///   - `"warn"`: `StubMismatch` diagnostics remain warnings.
///   - `"off"`: `StubMismatch` diagnostics are dropped entirely.
///
/// All other warnings are passed through unchanged. The function consumes the
/// input `Diagnostics` to avoid cloning individual diagnostics.
pub fn apply_strictness(diags: Diagnostics, config: &TyphonConfig) -> Diagnostics {
    let promote_unused_import = config.strictness.unused_import == "error";
    let methods_in_class_body = config.strictness.methods_in_class_body.as_str();
    let require_with = config.strictness.require_with.as_str();
    let require_with_default = require_with == "warn" || require_with.is_empty();
    let blocking_in_async = config.strictness.blocking_in_async.as_str();
    let blocking_in_async_default = blocking_in_async == "warn" || blocking_in_async.is_empty();
    let stub_check = config.strictness.stub_check.as_str();
    // Default is "error" — stub drift is promoted to error by default.
    // Only skip reclassification when the setting is explicitly "warn" or empty.
    let stub_check_default = stub_check == "warn" || stub_check.is_empty();
    // `exhaustive-match` defaults to "error". `NonExhaustiveMatch` is emitted
    // by the checker as an error, so honouring "warn"/"off" means reclassifying
    // it out of the errors bucket below.
    let exhaustive_match = config.strictness.exhaustive_match.as_str();
    let exhaustive_match_default = exhaustive_match == "error" || exhaustive_match.is_empty();
    if !promote_unused_import
        && (methods_in_class_body == "warn" || methods_in_class_body.is_empty())
        && require_with_default
        && blocking_in_async_default
        && stub_check_default
        && exhaustive_match_default
    {
        return diags;
    }
    let (errors, warnings) = diags.into_parts();
    let mut new_diags = Diagnostics::new();
    for err in errors {
        if matches!(err, TycError::NonExhaustiveMatch { .. }) && !exhaustive_match_default {
            match exhaustive_match {
                "off" => {}                            // drop entirely
                "warn" => new_diags.push_warning(err), // demote to warning
                _ => new_diags.push_error(err),        // any other value stays an error
            }
        } else {
            new_diags.push_error(err);
        }
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
        } else if matches!(warn, TycError::ResourceNotManaged { .. }) {
            match require_with {
                "off" => {} // drop the diagnostic entirely
                "error" => new_diags.push_error(warn),
                _ => new_diags.push_warning(warn),
            }
        } else if matches!(warn, TycError::BlockingInAsync { .. }) {
            match blocking_in_async {
                "off" => {} // drop the diagnostic entirely
                "error" => new_diags.push_error(warn),
                _ => new_diags.push_warning(warn),
            }
        } else if matches!(warn, TycError::StubMismatch { .. }) {
            match stub_check {
                "off" => {} // drop the diagnostic entirely
                "warn" => new_diags.push_warning(warn),
                // "error" (default) and any other value → promote to error
                _ => new_diags.push_error(warn),
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
    let mut visited = HashSet::new();
    collect_with_ext_impl(root, ext, acc, &mut visited, true)
}

fn collect_with_ext(root: &Path, ext: &str, acc: &mut Vec<PathBuf>) -> Result<()> {
    let mut visited = HashSet::new();
    collect_with_ext_impl(root, ext, acc, &mut visited, false)
}

/// Shared source-tree walk behind [`collect_with_ext`] and
/// [`collect_with_ext_filtered`]; `filtered` selects whether conventional
/// non-source directories are skipped.
///
/// `visited` holds the canonicalised path of every directory already
/// descended on this walk. Without it, a symlink pointing back up into the
/// tree is followed as if it were a real directory — `Path::is_dir()` calls
/// `stat`, not `lstat`, so it reports `true` for a link to a directory. The
/// only thing that stopped the walk at all was the kernel's 40-link
/// `ELOOP` ceiling, which bounds the depth but not the branching: one
/// back-link re-enumerated a three-file project under 41 distinct paths
/// (every diagnostic reported 41 times, every file checked 41 times), and two
/// made the walk effectively non-terminating.
///
/// Canonicalising also deduplicates a *legitimate* symlinked source
/// directory, so a linked shared-source tree is checked exactly once instead
/// of once per link.
fn collect_with_ext_impl(
    root: &Path,
    ext: &str,
    acc: &mut Vec<PathBuf>,
    visited: &mut HashSet<PathBuf>,
    filtered: bool,
) -> Result<()> {
    if root.is_file() {
        if root.extension().map(|e| e == ext).unwrap_or(false) {
            acc.push(root.to_path_buf());
        }
        return Ok(());
    }
    if root.is_dir() {
        // Identity is the canonical path, not the path we arrived by: two
        // different link paths to one directory must count as one visit. A
        // directory that cannot be canonicalised (permissions, a race) is
        // keyed by its literal path — worse deduplication, never a hang,
        // because the cycle case always canonicalises.
        let key = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        if !visited.insert(key) {
            return Ok(());
        }
        let entries = std::fs::read_dir(root)
            .map_err(|e| miette!("cannot read directory {}: {}", root.display(), e))?;
        let mut paths: Vec<PathBuf> = entries
            .map(|res| res.map(|e| e.path()))
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|e| miette!("cannot read directory entry in {}: {}", root.display(), e))?;
        paths.sort();
        for path in paths {
            if filtered && path.is_dir() {
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
            collect_with_ext_impl(&path, ext, acc, visited, filtered)?;
        }
    }
    Ok(())
}

/// Aggregate `pub *` package-facade shapes after `collect_project_shapes`.
///
/// When `<pkg>/__init__.ty` contains a `pub *` marker, the build pipeline
/// synthesises `from .sibling import name1, name2, …` lines so the
/// emitted Python re-exports every sibling module's `pub` surface. The
/// check pipeline never emits Python, so the synthetic imports never run
/// — and a downstream `from pkg import SomeClass` ends up looking for
/// `SomeClass` on `<pkg>/__init__`'s shape (which is empty by
/// construction). This function performs the equivalent aggregation at
/// the SHAPE level: for every `__init__.ty` carrying `pub *`, every
/// `pub`-marked name in every direct sibling module is merged into the
/// package's shape entry, plus every `pub`-aggregated name from each
/// direct sub-package's effective surface.
///
/// Without this, cross-module import flow that uses a `pub *` facade
/// silently loses sealed-union variants, interface shapes, function
/// signatures, and class fields — surfacing as misleading downstream
/// diagnostics (e.g. `tyc::missing_return` on a match the
/// exhaustiveness checker accepted, because the reachability pass
/// can't see the union's variant list). (Bug 2 from the v0.9.0 stress
/// pass on a multi-package app.)
///
/// Aggregation processes packages in deepest-first order so a parent
/// `pub *` __init__ picks up its sub-packages' already-aggregated
/// surface. Cross-sibling collisions are silently resolved by
/// first-write-wins — the build pipeline raises
/// `tyc::pub_name_collision` separately via `detect_pub_star_diagnostics`,
/// which both `tyc check` and `tyc build` already call.
pub fn aggregate_pub_star_shapes(
    shape_map: &mut HashMap<String, ModuleShapes>,
    paths: &[PathBuf],
    src_root: &str,
) {
    use std::collections::HashSet;

    // First pass (only __init__ files): identify packages with `pub *`,
    // and record the `pub_names` of every __init__ regardless of `pub *`
    // status. Reading non-init siblings is deferred to the second pass
    // so projects without any facade pay almost nothing here.
    let mut pub_star_dirs: HashSet<PathBuf> = HashSet::new();
    let mut pub_star_packages: Vec<(PathBuf, PathBuf)> = Vec::new(); // (pkg_dir, init_path)
    let mut init_pub_names: HashMap<PathBuf, Vec<String>> = HashMap::new();
    let mut init_has_pub_star: HashSet<PathBuf> = HashSet::new();
    for path in paths {
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem != "__init__" {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let prep = preprocess(&text);
        init_pub_names.insert(path.clone(), prep.pub_names);
        if !prep.pub_star_lines.is_empty() {
            init_has_pub_star.insert(path.clone());
            if let Some(dir) = path.parent() {
                pub_star_dirs.insert(dir.to_path_buf());
                pub_star_packages.push((dir.to_path_buf(), path.clone()));
            }
        }
    }
    if pub_star_packages.is_empty() {
        return;
    }
    // Second pass: only read non-init files whose parent dir is itself a
    // `pub *` package (the only files whose `pub_names` participate in
    // the merge). Skips most of the project tree.
    let mut module_pub_names: HashMap<PathBuf, Vec<String>> = HashMap::new();
    for path in paths {
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem == "__init__" {
            continue;
        }
        let Some(parent) = path.parent() else {
            continue;
        };
        if !pub_star_dirs.contains(parent) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let prep = preprocess(&text);
        module_pub_names.insert(path.clone(), prep.pub_names);
    }
    // Deepest-first ordering so a parent package picks up an already-
    // aggregated sub-package shape. Sorts by component count descending,
    // then lexicographic for determinism. Stable sort across runs.
    pub_star_packages.sort_by(|a, b| {
        let depth = |p: &Path| p.components().count();
        depth(&b.0).cmp(&depth(&a.0)).then(a.0.cmp(&b.0))
    });
    for (pkg_dir, init_path) in &pub_star_packages {
        let pkg_dotted = path_to_dotted(init_path, src_root);
        let mut merged = shape_map.remove(&pkg_dotted).unwrap_or_default();
        // Direct sibling .ty / .dty modules.
        for path in paths {
            let Some(parent) = path.parent() else {
                continue;
            };
            if parent != pkg_dir.as_path() {
                continue;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if stem == "__init__" {
                continue;
            }
            // Skip siblings whose `pub_names` couldn't be loaded (a
            // missing entry means the file failed to read in the second
            // pass — without a visibility filter, the `None` branch in
            // `merge_pub_visible` would leak every shape from that
            // module, including private ones). Surface a quiet skip
            // instead. (Gemini PR review #1.)
            let Some(visible_names) = module_pub_names.get(path) else {
                continue;
            };
            let sibling_dotted = path_to_dotted(path, src_root);
            let Some(sibling_shape) = shape_map.get(&sibling_dotted) else {
                continue;
            };
            let visible_set: HashSet<&str> = visible_names.iter().map(|s| s.as_str()).collect();
            merge_pub_visible(&mut merged, sibling_shape, Some(&visible_set));
        }
        // Direct sub-packages: any __init__ whose parent.parent == pkg_dir.
        // Filter the merge through the sub-package's effective public
        // surface so `pub *` doesn't accidentally re-export a private
        // declaration from the sub-package's __init__. Matches the
        // build pipeline's `effective_package_surface`.
        // (Codex / Copilot PR review #3 / #4.)
        for path in paths {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if stem != "__init__" {
                continue;
            }
            let Some(sub_dir) = path.parent() else {
                continue;
            };
            if sub_dir == pkg_dir.as_path() {
                continue;
            }
            if sub_dir.parent() != Some(pkg_dir.as_path()) {
                continue;
            }
            let sub_dotted = path_to_dotted(path, src_root);
            let Some(sub_shape) = shape_map.get(&sub_dotted) else {
                continue;
            };
            let mut visited: HashSet<PathBuf> = HashSet::new();
            let surface = effective_pub_surface(
                sub_dir,
                paths,
                &init_pub_names,
                &init_has_pub_star,
                &module_pub_names,
                &mut visited,
            );
            let visible_set: HashSet<&str> = surface.iter().map(|s| s.as_str()).collect();
            merge_pub_visible(&mut merged, sub_shape, Some(&visible_set));
        }
        shape_map.insert(pkg_dotted, merged);
    }
}

/// Compute the effective public surface name set for `pkg_dir`,
/// mirroring `tyc::commands::build::effective_package_surface`.
/// Returns the set of top-level names this package re-exports through
/// its `pub *` facade — or just its top-level `pub` names if no
/// `pub *`. Cycle-safe via the `visited` set. Used by
/// [`aggregate_pub_star_shapes`] to filter sub-package merges so a
/// parent `pub *` only re-exports its sub-packages' intended exports.
fn effective_pub_surface(
    pkg_dir: &Path,
    paths: &[PathBuf],
    init_pub_names: &HashMap<PathBuf, Vec<String>>,
    init_has_pub_star: &std::collections::HashSet<PathBuf>,
    module_pub_names: &HashMap<PathBuf, Vec<String>>,
    visited: &mut std::collections::HashSet<PathBuf>,
) -> std::collections::HashSet<String> {
    let mut out: std::collections::HashSet<String> = std::collections::HashSet::new();
    if !visited.insert(pkg_dir.to_path_buf()) {
        return out;
    }
    // Locate this package's __init__ (.ty or .dty) in `paths`.
    let init_path = paths.iter().find(|p| {
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        stem == "__init__" && p.parent() == Some(pkg_dir)
    });
    let Some(init_path) = init_path else {
        return out;
    };
    if let Some(names) = init_pub_names.get(init_path) {
        for n in names {
            out.insert(n.clone());
        }
    }
    if !init_has_pub_star.contains(init_path) {
        return out;
    }
    // Aggregate one level deeper.
    let mut sub_seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for sib_path in paths {
        let Some(sib_parent) = sib_path.parent() else {
            continue;
        };
        let stem = sib_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if sib_parent == pkg_dir {
            if stem == "__init__" {
                continue;
            }
            if let Some(pubs) = module_pub_names.get(sib_path) {
                for n in pubs {
                    out.insert(n.clone());
                }
            }
            continue;
        }
        if sib_parent.parent() == Some(pkg_dir) && sub_seen.insert(sib_parent.to_path_buf()) {
            for name in effective_pub_surface(
                sib_parent,
                paths,
                init_pub_names,
                init_has_pub_star,
                module_pub_names,
                visited,
            ) {
                out.insert(name);
            }
        }
    }
    out
}

/// Merge `src` into `dst`, keeping only names present in `visible`
/// (when `Some`). `None` re-exports every name in `src` — only safe
/// when the caller already filtered. First-write-wins on collisions so
/// the caller's pre-existing entries dominate. Mirrors the per-entry
/// merge done in `tyc-types::check_module_with_imports`.
fn merge_pub_visible(
    dst: &mut ModuleShapes,
    src: &ModuleShapes,
    visible: Option<&std::collections::HashSet<&str>>,
) {
    let include = |name: &str| -> bool {
        match visible {
            Some(set) => set.contains(name),
            None => true,
        }
    };
    for (name, shape) in &src.class_shapes {
        if include(name) {
            dst.class_shapes
                .entry(name.clone())
                .or_insert_with(|| shape.clone());
        }
    }
    for (name, tps) in &src.class_type_params {
        if include(name) {
            dst.class_type_params
                .entry(name.clone())
                .or_insert_with(|| tps.clone());
        }
    }
    for (name, arity) in &src.function_arities {
        if include(name) {
            dst.function_arities
                .entry(name.clone())
                .or_insert_with(|| arity.clone());
        }
    }
    for (name, variants) in &src.sealed_unions {
        if include(name) {
            dst.sealed_unions
                .entry(name.clone())
                .or_insert_with(|| variants.clone());
        }
    }
    for (name, runtime_checkable) in &src.interfaces {
        if include(name) {
            dst.interfaces
                .entry(name.clone())
                .or_insert(*runtime_checkable);
        }
    }
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

    // ── stub-check tests ──────────────────────────────────────────────────

    fn stub_mismatch_warning() -> TycError {
        TycError::stub_mismatch(
            "missing in implementation: foo",
            "<test>",
            "stub content",
            0,
            1,
        )
    }

    #[test]
    fn stub_check_error_promotes_stub_mismatch_to_error() {
        // Default: stub-check = "error" → StubMismatch promoted to error.
        let mut diags = Diagnostics::new();
        diags.push_warning(stub_mismatch_warning());
        let config = TyphonConfig::default(); // stub_check defaults to "error"
        let out = apply_strictness(diags, &config);
        assert_eq!(
            out.errors().len(),
            1,
            "StubMismatch must be an error with stub-check=error"
        );
        assert_eq!(out.warning_count(), 0);
        assert!(matches!(out.errors()[0], TycError::StubMismatch { .. }));
    }

    #[test]
    fn stub_check_warn_keeps_stub_mismatch_as_warning() {
        let mut diags = Diagnostics::new();
        diags.push_warning(stub_mismatch_warning());
        let mut config = TyphonConfig::default();
        config.strictness.stub_check = "warn".into();
        let out = apply_strictness(diags, &config);
        assert_eq!(
            out.errors().len(),
            0,
            "StubMismatch must stay warning with stub-check=warn"
        );
        assert_eq!(out.warning_count(), 1);
        assert!(matches!(out.warnings()[0], TycError::StubMismatch { .. }));
    }

    #[test]
    fn stub_check_off_drops_stub_mismatch() {
        let mut diags = Diagnostics::new();
        diags.push_warning(stub_mismatch_warning());
        let mut config = TyphonConfig::default();
        config.strictness.stub_check = "off".into();
        let out = apply_strictness(diags, &config);
        assert_eq!(
            out.errors().len(),
            0,
            "StubMismatch must be dropped with stub-check=off"
        );
        assert_eq!(out.warning_count(), 0);
    }

    #[test]
    fn stub_check_does_not_affect_other_warnings() {
        // stub-check = "off" must not drop unrelated warnings.
        let mut diags = Diagnostics::new();
        diags.push_warning(stub_mismatch_warning());
        diags.push_warning(method_in_class_body_warning());
        let mut config = TyphonConfig::default();
        config.strictness.stub_check = "off".into();
        let out = apply_strictness(diags, &config);
        // StubMismatch dropped; MethodInClassBody is warn by default → kept
        // (stub-check=error is the default so we also need to neutralise the
        // normal stub promotion; with stub-check=off the stub is gone; the
        // method warning stays as warning because methods-in-class-body="warn").
        assert_eq!(out.errors().len(), 0);
        assert_eq!(
            out.warning_count(),
            1,
            "MethodInClassBody warning must survive stub-check=off"
        );
        assert!(matches!(
            out.warnings()[0],
            TycError::MethodInClassBody { .. }
        ));
    }
}
