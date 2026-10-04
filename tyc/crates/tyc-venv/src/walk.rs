//! The symlink-safe source walk shared by the CLI and the language server.
//!
//! `tyc check` / `tyc build` / `tyc fmt` and `tyc lsp` all enumerate a
//! project's `.ty` / `.dty` files. The language server used to carry its own
//! copy of the walk with no loop guard: `ln -s . src/a; ln -s . src/b` made it
//! enumerate an exponential number of paths, so opening a file produced no
//! diagnostics and the server never recovered, while `tyc check` on the same
//! tree finished in under a second (W4-08). One walk now serves both.
//!
//! Guarantees:
//!
//! - **Terminates on symlink cycles.** Every directory is keyed by its
//!   canonical path; a directory already descended is not entered again, so a
//!   back-link (or two) costs one visit.
//! - **Stays in the tree.** A symlink whose target resolves outside the walk's
//!   root is skipped (`src/linked.ty -> ~/.bashrc`); links that stay inside
//!   (a shared source directory) are followed once.
//! - **Deterministic.** Entries are visited in sorted order.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Which directories the walk skips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirFilter {
    /// Descend into everything.
    None,
    /// Skip hidden (`.x`) directories and `__pycache__/`.
    Hidden,
    /// Skip hidden directories, `__pycache__/`, `tests/`, `.venv/` and
    /// `build/` — the directories that are never user-authored sources.
    NonSource,
    /// Skip only what nobody writes by hand: virtual environments (any
    /// directory holding a `pyvenv.cfg`, plus `.venv/`, `.tox/`, `.nox/`),
    /// VCS metadata, caches (`__pycache__/`, `.mypy_cache/`, …),
    /// `node_modules/` and `build/`. A project's `tests/` and other
    /// directories are kept — `tyc migrate` converts the whole tree.
    Generated,
}

impl DirFilter {
    fn skips(self, dir: &Path, name: &str) -> bool {
        match self {
            DirFilter::None => false,
            DirFilter::Hidden => name.starts_with('.') || name == "__pycache__",
            DirFilter::NonSource => {
                name.starts_with('.')
                    || name == "__pycache__"
                    || name == "tests"
                    || name == ".venv"
                    || name == "build"
            }
            DirFilter::Generated => {
                matches!(
                    name,
                    ".git"
                        | ".hg"
                        | ".svn"
                        | ".bzr"
                        | "__pycache__"
                        | ".venv"
                        | ".tox"
                        | ".nox"
                        | "node_modules"
                        | "build"
                ) || (name.starts_with('.') && name.ends_with("cache"))
                    || dir.join("pyvenv.cfg").is_file()
            }
        }
    }
}

/// An unreadable directory, reported by a strict walk.
#[derive(Debug)]
pub struct WalkError {
    pub path: PathBuf,
    pub cause: std::io::Error,
}

impl std::fmt::Display for WalkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cannot read directory {}: {}",
            self.path.display(),
            self.cause
        )
    }
}

impl std::error::Error for WalkError {}

/// A walk configuration: which extension to collect, which directories to
/// skip, and whether an unreadable directory is an error (`strict`, the CLI)
/// or skipped (the language server, which serves what it can).
#[derive(Debug, Clone, Copy)]
pub struct Walk<'a> {
    pub ext: &'a str,
    pub filter: DirFilter,
    pub strict: bool,
}

impl<'a> Walk<'a> {
    /// A lenient walk for `ext` that skips hidden directories.
    pub fn lenient(ext: &'a str) -> Self {
        Self {
            ext,
            filter: DirFilter::Hidden,
            strict: false,
        }
    }

    /// Every file under `root` (or `root` itself) whose extension is
    /// `self.ext`, in walk order. `on_escape` is called once for each symlink
    /// skipped because it leaves the tree.
    pub fn collect(
        &self,
        root: &Path,
        on_escape: &mut dyn FnMut(&Path),
    ) -> Result<Vec<PathBuf>, WalkError> {
        let mut acc = Vec::new();
        let mut visited = HashSet::new();
        let base = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        self.walk(root, &base, &mut acc, &mut visited, on_escape)?;
        Ok(acc)
    }

    /// [`Self::collect`] without the escape callback.
    pub fn collect_quiet(&self, root: &Path) -> Result<Vec<PathBuf>, WalkError> {
        self.collect(root, &mut |_| {})
    }

    fn walk(
        &self,
        root: &Path,
        base: &Path,
        acc: &mut Vec<PathBuf>,
        visited: &mut HashSet<PathBuf>,
        on_escape: &mut dyn FnMut(&Path),
    ) -> Result<(), WalkError> {
        if symlink_escapes(root, base) {
            on_escape(root);
            return Ok(());
        }
        if root.is_file() {
            if root.extension().is_some_and(|e| e == self.ext) {
                acc.push(root.to_path_buf());
            }
            return Ok(());
        }
        if !root.is_dir() {
            return Ok(());
        }
        // Identity is the canonical path, not the path we arrived by: two
        // different link paths to one directory count as one visit. A
        // directory that cannot be canonicalised (permissions, a race) is
        // keyed by its literal path — worse deduplication, never a hang,
        // because the cycle case always canonicalises.
        let key = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        if !visited.insert(key) {
            return Ok(());
        }
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(cause) if self.strict => {
                return Err(WalkError {
                    path: root.to_path_buf(),
                    cause,
                })
            }
            Err(_) => return Ok(()),
        };
        let mut paths: Vec<PathBuf> = Vec::new();
        for entry in entries {
            match entry {
                Ok(e) => paths.push(e.path()),
                Err(cause) if self.strict => {
                    return Err(WalkError {
                        path: root.to_path_buf(),
                        cause,
                    })
                }
                Err(_) => {}
            }
        }
        paths.sort();
        for path in paths {
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if self.filter.skips(&path, name) {
                        continue;
                    }
                }
            }
            self.walk(&path, base, acc, visited, on_escape)?;
        }
        Ok(())
    }
}

/// Whether `path` is a symlink whose target resolves outside `base` (the
/// canonical root of the walk). A checked-out tree can carry a symlink to
/// anywhere the user can read or write — `src/linked.ty -> ~/.bashrc` — and
/// git preserves it, so `tyc fmt src/` would rewrite the target and
/// `tyc check src/` would walk it. Links that stay inside the tree (a shared
/// source directory) are still followed; links that leave it are skipped.
pub fn symlink_escapes(path: &Path, base: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.file_type().is_symlink() {
        return false;
    }
    match std::fs::canonicalize(path) {
        Ok(target) => !target.starts_with(base),
        // A dangling link resolves nowhere useful; skip it either way.
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn symlink_loops_terminate_and_list_each_file_once() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("main.ty"), "x = 1\n").unwrap();
        std::os::unix::fs::symlink(".", src.join("a")).unwrap();
        std::os::unix::fs::symlink(".", src.join("b")).unwrap();
        let files = Walk::lenient("ty").collect_quiet(&src).unwrap();
        assert_eq!(files, vec![src.join("main.ty")]);
    }

    #[cfg(unix)]
    #[test]
    fn links_leaving_the_tree_are_skipped_and_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("victim.ty"), "x = 1\n").unwrap();
        std::os::unix::fs::symlink(&outside, src.join("out")).unwrap();
        let mut escaped = Vec::new();
        let files = Walk::lenient("ty")
            .collect(&src, &mut |p| escaped.push(p.to_path_buf()))
            .unwrap();
        assert!(files.is_empty(), "{files:?}");
        assert_eq!(escaped, vec![src.join("out")]);
    }

    #[test]
    fn filters_skip_their_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for dir in [".hidden", "__pycache__", "tests", "build", "pkg"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join("m.ty"), "").unwrap();
        }
        let all = |filter| {
            Walk {
                ext: "ty",
                filter,
                strict: true,
            }
            .collect_quiet(root)
            .unwrap()
            .len()
        };
        assert_eq!(all(DirFilter::None), 5);
        assert_eq!(all(DirFilter::Hidden), 3);
        assert_eq!(all(DirFilter::NonSource), 1);
        // `.hidden`, `tests` and `pkg`: only `__pycache__` and `build` are
        // generated.
        assert_eq!(all(DirFilter::Generated), 3);
    }

    #[test]
    fn the_generated_filter_skips_environments_caches_and_vcs_only() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let skipped = [
            ".git",
            ".venv",
            "env",
            ".tox",
            "node_modules",
            ".mypy_cache",
            ".pytest_cache",
            "__pycache__",
            "build",
        ];
        let kept = ["tests", "pkg", ".github", "docs"];
        for dir in skipped.iter().chain(kept.iter()) {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join("m.py"), "").unwrap();
        }
        // A virtual environment under any name is recognised by its marker.
        std::fs::write(root.join("env").join("pyvenv.cfg"), "home = /usr\n").unwrap();
        let files = Walk {
            ext: "py",
            filter: DirFilter::Generated,
            strict: true,
        }
        .collect_quiet(root)
        .unwrap();
        let mut dirs: Vec<String> = files
            .iter()
            .map(|f| {
                f.parent()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        dirs.sort();
        assert_eq!(dirs, vec![".github", "docs", "pkg", "tests"]);
    }
}
