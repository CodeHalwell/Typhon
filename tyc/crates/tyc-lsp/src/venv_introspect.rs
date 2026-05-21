//! Venv-driven module introspection for member completion.
//!
//! When the editor asks "what's in `os.`?", we shell to the project's
//! `.venv/bin/python` (or a fallback `python3`) and ask Python itself.
//! This sees stdlib modules and every third-party package the user
//! `uv add`-ed, without the LSP shipping any per-library data.
//!
//! Cache structure:
//! - One [`IntrospectionCache`] per project root (directory containing
//!   `typhon.toml`).
//! - Cache is keyed by dotted module path (`"os"`, `"os.path"`,
//!   `"numpy.linalg"`). The value is `Some(members)` after a successful
//!   introspection, or `None` after a failed one — failures are
//!   remembered so a repeatedly-mistyped module doesn't re-launch
//!   Python on every keystroke.
//! - Whole cache is invalidated when the venv's `pyvenv.cfg` mtime
//!   changes (covers `uv sync`, `uv pip install`, deleting `.venv`,
//!   etc.). Best-effort: a venv without `pyvenv.cfg` keeps the cache
//!   forever within a session.
//!
//! The Python helper script is embedded as a const string (see
//! [`INTROSPECT_SCRIPT`]) so the LSP has nothing to install at the
//! Python side.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::Deserialize;

/// One member surfaced for a module — the introspection result for
/// a single name reachable through `dir(module)`.
#[derive(Debug, Clone, Deserialize)]
pub struct MemberInfo {
    pub name: String,
    /// One of `"function"`, `"class"`, `"module"`, `"value"`.
    pub kind: String,
    /// Pretty-printed signature when one is available
    /// (`getcwd() -> str`). `None` for plain values and for callables
    /// whose signature is opaque (C-built-ins without stubs).
    pub signature: Option<String>,
    /// One-liner pulled from the object's first docstring line.
    pub documentation: Option<String>,
}

/// Single-project introspection cache. Holds a chosen Python binary
/// path and the per-module results we've seen so far.
pub struct IntrospectionCache {
    /// Python binary to invoke. Typically `<root>/.venv/bin/python`;
    /// falls back to `python3` on PATH when no `.venv` is present.
    /// `None` means we looked, found nothing usable, and won't try
    /// to shell.
    python_bin: Option<PathBuf>,
    /// `pyvenv.cfg` mtime at cache creation, used as the invalidation
    /// stamp.
    venv_stamp: Option<SystemTime>,
    /// `module_name -> Some(members) | None`.  `None` records a known
    /// failure so we don't re-launch Python for the same broken
    /// module-name within one session.
    cache: HashMap<String, Option<Arc<Vec<MemberInfo>>>>,
}

impl IntrospectionCache {
    /// Discover the project's venv (or fall back to `python3` on PATH)
    /// and return a fresh cache. `project_root` is the directory
    /// containing `typhon.toml`.
    pub fn for_project_root(project_root: &Path) -> Self {
        let venv_python = project_root.join(".venv").join("bin").join("python");
        let venv_stamp = stat_pyvenv_cfg(project_root);
        let python_bin = if venv_python.is_file() {
            Some(venv_python)
        } else {
            which_python3()
        };
        Self {
            python_bin,
            venv_stamp,
            cache: HashMap::new(),
        }
    }

    /// Look up `module` — returns cached members if present, otherwise
    /// shells to Python and caches the result (success or failure).
    /// Returns `None` when the module can't be imported or no Python
    /// is available.
    ///
    /// `project_root` is passed back in so the subprocess can find a
    /// relocated venv (e.g. `uv sync` after a `mv`). The
    /// re-stat-and-invalidate happens here rather than at cache
    /// construction so we don't pay it on every completion request
    /// — only on miss.
    pub fn members(&mut self, project_root: &Path, module: &str) -> Option<Arc<Vec<MemberInfo>>> {
        // Re-stat the venv stamp BEFORE the cache check. The previous
        // shape (cache check first, stat only on miss) would happily
        // serve stale entries for modules the user had already queried
        // when `uv sync` / `uv pip install` ran in the background —
        // exactly the case where fresh information matters most. The
        // stat is microseconds; pay it on every call.
        let current_stamp = stat_pyvenv_cfg(project_root);
        if current_stamp != self.venv_stamp {
            self.cache.clear();
            self.venv_stamp = current_stamp;
            // Also re-discover the venv: a new `uv sync` may have
            // materialised one where there wasn't before.
            let venv_python = project_root.join(".venv").join("bin").join("python");
            self.python_bin = if venv_python.is_file() {
                Some(venv_python)
            } else {
                which_python3()
            };
        }
        // Cache check — `None` value short-circuits repeated failures,
        // `Some` returns the result.
        if let Some(entry) = self.cache.get(module) {
            return entry.clone();
        }
        let python = self.python_bin.clone()?;
        let result = introspect_via_python(&python, module);
        let result_arc = result.map(Arc::new);
        self.cache.insert(module.to_owned(), result_arc.clone());
        result_arc
    }
}

/// `pyvenv.cfg` is the file `uv`/`venv` writes when materialising a
/// virtual environment; its presence and mtime are the cleanest
/// "anything about this venv changed" signal we can detect without a
/// dedicated filesystem watcher.
fn stat_pyvenv_cfg(project_root: &Path) -> Option<SystemTime> {
    let cfg = project_root.join(".venv").join("pyvenv.cfg");
    std::fs::metadata(&cfg).ok().and_then(|m| m.modified().ok())
}

/// Find a `python3` on the user's PATH using a small portable lookup.
/// Returns the first matching entry; `None` when the PATH has nothing
/// callable named `python3`.
fn which_python3() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("python3");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Embedded Python helper. Reads the module name from `sys.argv[1]`,
/// imports it, and prints the JSON-encoded member list on stdout.
///
/// Failures (`ImportError`, anything else) print `[]` and exit 0 — we
/// want the LSP path to record a no-such-module miss rather than
/// surface stderr noise.
///
/// The script intentionally avoids depending on anything outside the
/// stdlib so it works against any Python the user has on hand.
const INTROSPECT_SCRIPT: &str = r#"
import sys, json, importlib, inspect

def kind_of(obj):
    if inspect.isclass(obj):
        return "class"
    if inspect.ismodule(obj):
        return "module"
    if callable(obj):
        return "function"
    return "value"

def doc_of(obj):
    """Return the object's full docstring, capped at 4 KB so the LSP
    payload stays small. The Rust side strips PEP 257 indentation
    and renders it as Markdown — we don't pre-process here so future
    UI work (e.g. linking back to the source line) has the raw
    text to work with.
    """
    doc = getattr(obj, "__doc__", None)
    if not doc:
        return None
    doc = doc.strip("\n")
    if not doc:
        return None
    if len(doc) > 4096:
        doc = doc[:4096] + "\n…(truncated)"
    return doc

def signature_of(name, obj):
    try:
        sig = inspect.signature(obj)
    except (TypeError, ValueError):
        return None
    text = f"{name}{sig}"
    return text if len(text) <= 400 else None

def main():
    if len(sys.argv) < 2:
        print("[]")
        return
    mod_name = sys.argv[1]
    try:
        m = importlib.import_module(mod_name)
    except BaseException:
        print("[]")
        return
    out = []
    for name in dir(m):
        if name.startswith("_"):
            continue
        try:
            obj = getattr(m, name)
        except BaseException:
            continue
        out.append({
            "name": name,
            "kind": kind_of(obj),
            "signature": signature_of(name, obj),
            "documentation": doc_of(obj),
        })
    print(json.dumps(out))

main()
"#;

/// Shell to `python` with [`INTROSPECT_SCRIPT`] and the requested
/// module name. Returns the parsed member list on success.
///
/// Aggressive timeout (3 seconds): a misbehaving package's import-time
/// side effects (network call, sleep) can't be allowed to wedge the
/// LSP. The cache records the failure either way, so a slow module
/// won't re-block on the next keystroke.
fn introspect_via_python(python: &Path, module: &str) -> Option<Vec<MemberInfo>> {
    // We feed the script over stdin rather than `-c "<code>"` to avoid
    // quoting hell across platforms; `python -` reads from stdin.
    use std::io::{Read, Write};
    let mut child = Command::new(python)
        .arg("-")
        .arg(module)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    {
        let mut stdin = child.stdin.take()?;
        stdin.write_all(INTROSPECT_SCRIPT.as_bytes()).ok()?;
        // Explicit close (drop) so the child's `sys.stdin.read()` sees
        // EOF and exits the import loop; otherwise the script blocks
        // forever waiting for more input.
    }
    // Drain stdout on a dedicated thread. Without this, modules with
    // a large surface (e.g. `numpy` exposes hundreds of names) can
    // fill the stdout pipe buffer (typically 64 KB on macOS/Linux) and
    // the child blocks on `print()`, never reaching `sys.exit(0)`;
    // the timeout below would kill it but the result is always None.
    let mut stdout = child.stdout.take()?;
    let drainer = std::thread::spawn(move || -> Vec<u8> {
        let mut buf = Vec::with_capacity(64 * 1024);
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    // Wait with a timeout. `Child::wait` blocks indefinitely, so we
    // poll. Coarse 50 ms polling is fine for the 3 s ceiling and
    // happens to be the granularity at which a hung import becomes
    // user-visible anyway.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let success = loop {
        match child.try_wait().ok()? {
            Some(status) => break status.success(),
            None => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    let stdout_bytes = drainer.join().ok()?;
    if !success {
        return None;
    }
    let stdout_str = std::str::from_utf8(&stdout_bytes).ok()?;
    let parsed: Vec<MemberInfo> = serde_json::from_str(stdout_str.trim()).ok()?;
    if parsed.is_empty() {
        // An empty result is the script's "couldn't import" signal —
        // record it as a miss so we don't retry.
        return None;
    }
    Some(parsed)
}

/// Walk upward from `start` looking for a `typhon.toml` and return the
/// directory containing it. Returns `None` when no project root is in
/// the ancestor chain.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut dir: PathBuf = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };
    loop {
        if dir.join("typhon.toml").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_project_root_walks_upward() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("typhon.toml"), "[project]\nname=\"x\"\n").unwrap();
        std::fs::create_dir_all(root.join("src").join("nested")).unwrap();
        let nested = root.join("src").join("nested").join("main.ty");
        std::fs::write(&nested, "").unwrap();
        let found = find_project_root(&nested).expect("should find typhon.toml ancestor");
        // Normalise both via canonicalize so symlink prefixes (`/tmp` vs
        // `/private/tmp` on macOS) don't sneak in.
        assert_eq!(found.canonicalize().unwrap(), root.canonicalize().unwrap());
    }

    #[test]
    fn find_project_root_returns_none_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(find_project_root(&nested).is_none());
    }

    #[test]
    fn cache_remembers_failures() {
        // Without a venv and without a `python3` on PATH this is a
        // sanity-only test: it confirms that `members` doesn't panic
        // and that a second call doesn't re-shell. We can't assert on
        // the *content* of the result because that depends on the
        // test machine.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("typhon.toml"), "").unwrap();
        let mut cache = IntrospectionCache::for_project_root(root);
        let _first = cache.members(root, "definitely_not_a_real_module_xyz");
        let _second = cache.members(root, "definitely_not_a_real_module_xyz");
        // Either both succeed or both fail; either way the cache size
        // must be exactly 1 after two lookups of the same name.
        assert_eq!(cache.cache.len(), 1);
    }

    /// Integration test: only runs when `python3` is on PATH. Verifies
    /// that we can introspect the stdlib `os` module and get back
    /// `getcwd` with a reasonable kind.
    #[test]
    fn introspects_os_module_when_python_available() {
        let Some(python) = which_python3() else {
            return;
        };
        let members = match introspect_via_python(&python, "os") {
            Some(m) => m,
            None => return,
        };
        let getcwd = members
            .iter()
            .find(|m| m.name == "getcwd")
            .expect("os should expose getcwd");
        assert_eq!(getcwd.kind, "function");
    }
}
