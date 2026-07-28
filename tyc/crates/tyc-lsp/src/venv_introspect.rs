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

use std::collections::{HashMap, HashSet};
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
    /// For class members: the dotted base-class names (sans `object`),
    /// in MRO order. `None` for non-classes and for classes whose MRO
    /// probe raised. Used by the hover renderer to surface inheritance
    /// (`Module → torch.nn.Module → object`) — a noticeable upgrade
    /// over Pylance's defaults for ML packages.
    #[serde(default)]
    pub bases: Option<Vec<String>>,
    /// For class members: a short list of the public methods declared
    /// on the class (capped server-side). `None` for non-classes; an
    /// empty list when the class has no public methods. Lets the
    /// hover popover hint at the API surface without forcing the user
    /// to navigate to the source.
    #[serde(default)]
    pub methods: Option<Vec<String>>,
}

/// Why a particular module introspection failed. Surfaced through
/// [`IntrospectionCache::last_failure`] so the LSP can offer the user
/// a hint ("install torch in `.venv`", "`.venv/bin/python` not found")
/// instead of silently returning an empty completion list.
#[derive(Debug, Clone)]
pub enum IntrospectionFailure {
    /// No Python interpreter was discovered — no `.venv/bin/python`
    /// and no `python3` on `PATH`.
    NoPython,
    /// The subprocess ran but reported an empty member list, which
    /// the script uses as the universal "couldn't import" signal
    /// (caught at `import`-time, attribute-error during `dir`, etc).
    /// `python_bin` is captured so the hint can name the offending
    /// interpreter.
    ImportFailed { python_bin: PathBuf },
    /// The subprocess exceeded the per-call timeout. Rare but real
    /// for packages with slow C-extension init.
    Timeout { python_bin: PathBuf },
    /// The subprocess failed to spawn or exited with a non-zero
    /// status before producing JSON.
    SpawnFailed { python_bin: PathBuf },
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
    /// Top-level import names this cache is permitted to introspect: the
    /// project's declared dependencies (exactly the set `tyc check` uses)
    /// plus the Python stdlib.
    ///
    /// Introspection *imports* the named module in a subprocess, running its
    /// import-time code. Without this gate, opening a `.ty` file that merely
    /// names a module was enough to execute it — so a repository could ship
    /// an `evil.py` beside a `.ty` that imports it and get code execution
    /// from the act of opening the folder in an editor. The CLI has always
    /// had this gate; this copy did not.
    allowed_top_level: HashSet<String>,
    /// Working directory for the introspection subprocess. Deliberately
    /// **not** the project root: with the project root as cwd, Python's
    /// `sys.path[0]` is the project itself, so an attacker-controlled
    /// `<root>/json.py` shadows the stdlib module of the same name and runs
    /// instead of it. An empty scratch directory has nothing to shadow with.
    cwd: PathBuf,
    /// Per-module failure reason (kept only for cache entries whose
    /// value is `None`). Lets the LSP surface "torch.nn import
    /// failed in <python>" diagnostics instead of silently showing
    /// zero completions.
    failures: HashMap<String, IntrospectionFailure>,
}

impl IntrospectionCache {
    /// Discover the project's venv (or fall back to `python3` on PATH)
    /// and return a fresh cache. `project_root` is the directory
    /// containing `typhon.toml`.
    pub fn for_project_root(project_root: &Path) -> Self {
        let venv_stamp = stat_pyvenv_cfg(project_root);
        Self {
            // Shared with the CLI: honours `TYC_NO_INTROSPECT` and probes the
            // Windows venv layout. This used to be a second, weaker copy that
            // did neither.
            python_bin: tyc_venv::discover_python(project_root),
            venv_stamp,
            cache: HashMap::new(),
            failures: HashMap::new(),
            allowed_top_level: introspection_allow_list(project_root),
            cwd: scratch_cwd(),
        }
    }

    /// Latest known failure reason for `module`, if introspection has
    /// been attempted and missed. Returns `None` when introspection
    /// succeeded or has not been attempted. Used by the LSP hover to
    /// explain why a third-party import yields no completions.
    pub fn last_failure(&self, module: &str) -> Option<&IntrospectionFailure> {
        self.failures.get(module)
    }

    /// Return the chosen Python binary, if any. Useful for failure
    /// messages ("install torch into `<path>`").
    pub fn python_bin(&self) -> Option<&Path> {
        self.python_bin.as_deref()
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
            self.failures.clear();
            self.venv_stamp = current_stamp;
            // Also re-discover the venv: a new `uv sync` may have
            // materialised one where there wasn't before. A `uv sync` also
            // means new packages, so refresh the allow-list with it.
            self.python_bin = tyc_venv::discover_python(project_root);
            self.allowed_top_level = introspection_allow_list(project_root);
        }
        // Allow-list gate. Refuse to import anything the project has not
        // declared as a dependency and that is not stdlib — importing it is
        // arbitrary code execution, and a completion request is not consent
        // to run a stranger's module. Recorded as a plain miss (no failure
        // hint) so the editor shows no completions rather than an error.
        let top = module.split('.').next().unwrap_or(module);
        if !may_introspect(&self.allowed_top_level, top) {
            return None;
        }
        // Cache check — `None` value short-circuits repeated failures,
        // `Some` returns the result.
        if let Some(entry) = self.cache.get(module) {
            return entry.clone();
        }
        let Some(python) = self.python_bin.clone() else {
            self.failures
                .insert(module.to_owned(), IntrospectionFailure::NoPython);
            self.cache.insert(module.to_owned(), None);
            return None;
        };
        let (result, failure) = introspect_via_python(&python, &self.cwd, module);
        let result_arc = result.map(Arc::new);
        if result_arc.is_none() {
            if let Some(reason) = failure {
                self.failures.insert(module.to_owned(), reason);
            }
        } else {
            self.failures.remove(module);
        }
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

/// The project's declared dependencies — the same allow-list `tyc check`
/// computes from `[dependencies]` + `[dev-dependencies]`, expanded through
/// installed `.dist-info` metadata so packages whose import root differs from
/// their PyPI name (`beautifulsoup4` → `bs4`) are covered.
fn introspection_allow_list(project_root: &Path) -> HashSet<String> {
    tyc_venv::allowed_top_level_from_project(project_root)
}

/// True when the editor may introspect `top`.
///
/// Two arms, and the split is the trust boundary. **Stdlib** is always
/// allowed: `os.` / `json.` completions are the common case, no project
/// declares those as dependencies, and importing them is not a trust decision
/// — it is code that ships with the interpreter the user already chose to run.
/// **Declared dependencies** are allowed because declaring a package in
/// `typhon.toml` is the act of consent. Everything else — including a module
/// that merely happens to sit next to the file being edited — is refused.
fn may_introspect(allowed: &HashSet<String>, top: &str) -> bool {
    tyc_analyse::perf::is_stdlib_top_level(top) || allowed.contains(top)
}

/// A directory to run the introspection subprocess in that contains nothing
/// importable. Python prepends the script's directory (for `python -`, the
/// current directory) to `sys.path`, so running in the project root lets any
/// `.py` file there shadow the module being introspected. The system temp
/// directory is not ideal either but is not attacker-chosen per project;
/// falling back to the root directory keeps the call infallible.
fn scratch_cwd() -> PathBuf {
    let dir = std::env::temp_dir().join("tyc-lsp-introspect");
    if std::fs::create_dir_all(&dir).is_ok() {
        dir
    } else {
        std::env::temp_dir()
    }
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

    For classes whose body carries no docstring (a common pattern when
    the author hung the documentation on `__init__` instead), fall
    back to `__init__.__doc__` so the hover popover still shows
    parameter docs. Without this fallback, the hover for a class
    like agent_framework.Agent would degrade to "📦 from …" with no
    Markdown body — visibly less useful than the same hover in
    Pylance.
    """
    doc = getattr(obj, "__doc__", None)
    if (not doc) and inspect.isclass(obj):
        init = getattr(obj, "__init__", None)
        if init is not None:
            doc = getattr(init, "__doc__", None)
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
    # Bumped from 400 → 1024: a Pydantic / SQLAlchemy / Django model
    # constructor can carry 20+ typed parameters and overflow the
    # tighter cap. The LSP payload stays well under wire-protocol
    # limits at this size; hover renders the full signature in a
    # code block where horizontal scroll is acceptable.
    return text if len(text) <= 1024 else None

def bases_of(cls):
    """Return the dotted base-class names in MRO order, skipping the
    class itself and skipping `object` (the universal base adds no
    information). Capped at 5 entries so the hover doesn't get a
    massive multi-line inheritance trail for Pydantic-style mixin
    chains. Returns None on any failure so the hover renderer can
    omit the section cleanly."""
    try:
        mro = inspect.getmro(cls)
    except BaseException:
        return None
    out = []
    for base in mro[1:]:
        if base is object:
            continue
        mod = getattr(base, "__module__", "") or ""
        qualname = getattr(base, "__qualname__", "") or getattr(base, "__name__", "") or ""
        if not qualname:
            continue
        # Hide builtins ('builtins.Exception' is noisier than just
        # 'Exception'); keep the dotted form for third-party so the
        # user can tell `torch.nn.Module` from `tensorflow.Module`.
        if mod and mod != "builtins":
            out.append(f"{mod}.{qualname}")
        else:
            out.append(qualname)
        if len(out) >= 5:
            break
    return out

def methods_of(cls):
    """Return up to 12 public method names declared on `cls` (skipping
    inherited methods and dunders). Stable-ordered as Python sees
    them so repeated hovers show the same list. Returns None on
    failure so the hover renderer omits the section."""
    try:
        own = [n for n in vars(cls)
               if not n.startswith("_") and callable(vars(cls).get(n))]
    except BaseException:
        return None
    own.sort()
    return own[:12]

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
        entry = {
            "name": name,
            "kind": kind_of(obj),
            "signature": signature_of(name, obj),
            "documentation": doc_of(obj),
        }
        if inspect.isclass(obj):
            entry["bases"] = bases_of(obj)
            entry["methods"] = methods_of(obj)
        out.append(entry)
    print(json.dumps(out))

main()
"#;

/// Shell to `python` with [`INTROSPECT_SCRIPT`] and the requested
/// module name. Returns the parsed member list on success.
///
/// Timeout (10 seconds): heavy packages with non-trivial import-time
/// initialisation (`torch.nn` triggers C extension loading and CUDA
/// probing in the multi-hundred-millisecond range) routinely exceeded
/// the previous 3 s ceiling on the first call, leaving the user with
/// no completions for the most common ML/scientific imports. The
/// cache records the failure either way, so a slow module won't
/// re-block on the next keystroke.
fn introspect_via_python(
    python: &Path,
    cwd: &Path,
    module: &str,
) -> (Option<Vec<MemberInfo>>, Option<IntrospectionFailure>) {
    // We feed the script over stdin rather than `-c "<code>"` to avoid
    // quoting hell across platforms; `python -` reads from stdin.
    use std::io::{Read, Write};
    let spawn_failure = || IntrospectionFailure::SpawnFailed {
        python_bin: python.to_path_buf(),
    };
    let mut child = match Command::new(python)
        .arg("-")
        .arg(module)
        // Run outside the project tree. With the project root as cwd,
        // Python puts it on `sys.path`, so a file named after a stdlib
        // module (`<root>/json.py`) is imported in place of the real one.
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return (None, Some(spawn_failure())),
    };
    {
        let Some(mut stdin) = child.stdin.take() else {
            return (None, Some(spawn_failure()));
        };
        if stdin.write_all(INTROSPECT_SCRIPT.as_bytes()).is_err() {
            return (None, Some(spawn_failure()));
        }
        // Explicit close (drop) so the child's `sys.stdin.read()` sees
        // EOF and exits the import loop; otherwise the script blocks
        // forever waiting for more input.
    }
    // Drain stdout on a dedicated thread. Without this, modules with
    // a large surface (e.g. `numpy` exposes hundreds of names) can
    // fill the stdout pipe buffer (typically 64 KB on macOS/Linux) and
    // the child blocks on `print()`, never reaching `sys.exit(0)`;
    // the timeout below would kill it but the result is always None.
    let Some(mut stdout) = child.stdout.take() else {
        return (None, Some(spawn_failure()));
    };
    let drainer = std::thread::spawn(move || -> Vec<u8> {
        let mut buf = Vec::with_capacity(64 * 1024);
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    // Wait with a timeout. `Child::wait` blocks indefinitely, so we
    // poll. Coarse 50 ms polling is fine for the 10 s ceiling and
    // happens to be the granularity at which a hung import becomes
    // user-visible anyway.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut timed_out = false;
    let success = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return (None, Some(spawn_failure())),
        }
    };
    let stdout_bytes = drainer.join().unwrap_or_default();
    if timed_out {
        return (
            None,
            Some(IntrospectionFailure::Timeout {
                python_bin: python.to_path_buf(),
            }),
        );
    }
    if !success {
        return (None, Some(spawn_failure()));
    }
    let stdout_str = match std::str::from_utf8(&stdout_bytes) {
        Ok(s) => s,
        Err(_) => return (None, Some(spawn_failure())),
    };
    let parsed: Vec<MemberInfo> = match serde_json::from_str(stdout_str.trim()) {
        Ok(p) => p,
        Err(_) => return (None, Some(spawn_failure())),
    };
    if parsed.is_empty() {
        // An empty result is the script's "couldn't import" signal —
        // record it as a miss so we don't retry. The typical cause is
        // the package being absent from the chosen interpreter; the
        // LSP surfaces this through the hover hint so the user can
        // install it.
        return (
            None,
            Some(IntrospectionFailure::ImportFailed {
                python_bin: python.to_path_buf(),
            }),
        );
    }
    (Some(parsed), None)
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
        // Use a stdlib name: an undeclared, non-stdlib module is now refused
        // by the allow-list before it ever reaches the cache, so it would
        // never produce an entry to assert on.
        let _first = cache.members(root, "os");
        let _second = cache.members(root, "os");
        // Either both succeed or both fail; either way the cache size
        // must be exactly 1 after two lookups of the same name.
        assert_eq!(cache.cache.len(), 1);
    }

    #[test]
    fn undeclared_module_is_never_introspected() {
        // The security property: naming a module in a `.ty` file must not be
        // enough to make the editor import it. Only stdlib and the project's
        // declared dependencies are importable.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(
            root.join("typhon.toml"),
            "[project]\nname = \"p\"\n\n[dependencies]\nrich = \"*\"\n",
        )
        .unwrap();
        let mut cache = IntrospectionCache::for_project_root(root);

        // Not declared, not stdlib → refused, and nothing is cached (so no
        // subprocess was spawned and none will be on a repeat lookup).
        assert!(cache.members(root, "evil_local_module").is_none());
        assert!(cache.cache.is_empty());

        // A submodule of an undeclared package is refused on its top level.
        assert!(cache.members(root, "evil_local_module.sub").is_none());
        assert!(cache.cache.is_empty());

        // Declared and stdlib names pass the gate (whether the subsequent
        // import succeeds depends on the machine, so only the gate is
        // asserted — a cache entry means we got past it).
        assert!(may_introspect(&cache.allowed_top_level, "rich"));
        assert!(may_introspect(&cache.allowed_top_level, "os"));
        assert!(!may_introspect(
            &cache.allowed_top_level,
            "evil_local_module"
        ));
    }

    #[test]
    fn tyc_no_introspect_disables_the_editor_path_too() {
        // `SECURITY.md` documents `TYC_NO_INTROSPECT` as disabling dependency
        // introspection. It used to disable only the CLI's — the editor kept
        // executing dependency import-time code past the kill-switch.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("typhon.toml"), "").unwrap();

        // SAFETY: single-threaded test body; the var is removed before return.
        unsafe { std::env::set_var("TYC_NO_INTROSPECT", "1") };
        let cache = IntrospectionCache::for_project_root(root);
        let disabled = cache.python_bin().is_none();
        unsafe { std::env::remove_var("TYC_NO_INTROSPECT") };

        assert!(
            disabled,
            "TYC_NO_INTROSPECT must leave the LSP with no interpreter to shell to"
        );
    }

    /// Integration test: only runs when `python3` is on PATH. Verifies
    /// that we can introspect the stdlib `os` module and get back
    /// `getcwd` with a reasonable kind.
    #[test]
    fn introspects_os_module_when_python_available() {
        let Some(python) = tyc_venv::which_python3_for_test() else {
            return;
        };
        let (members, failure) = introspect_via_python(&python, &scratch_cwd(), "os");
        let members = match members {
            Some(m) => m,
            None => return,
        };
        assert!(failure.is_none(), "expected no failure on success");
        let getcwd = members
            .iter()
            .find(|m| m.name == "getcwd")
            .expect("os should expose getcwd");
        assert_eq!(getcwd.kind, "function");
    }

    #[test]
    fn records_failure_for_nonexistent_module() {
        // Sanity check: failed introspection captures a reason instead
        // of silently returning a bare `None`, so the LSP can offer
        // the user a useful "did you mean to install …?" hint.
        let Some(python) = tyc_venv::which_python3_for_test() else {
            return;
        };
        let (members, failure) = introspect_via_python(
            &python,
            &scratch_cwd(),
            "definitely_not_a_real_module_xyz_typhon",
        );
        assert!(members.is_none());
        assert!(matches!(
            failure,
            Some(IntrospectionFailure::ImportFailed { .. })
        ));
    }
}
