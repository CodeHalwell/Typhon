//! Venv-driven signature introspection for the type checker.
//!
//! When a project imports a third-party Python package that ships no
//! `.dty` stub (the common case for things installed via `uv add`),
//! the checker has no signature info for its classes / functions and
//! call-site arity checks silently pass — every callable degrades to
//! `Type::Unknown` and the runtime is left to discover the missing
//! required argument. The result is exactly what the user hit:
//!
//! ```text
//! $ tyc check
//! checked 4 file(s) — no errors
//! $ uv run agent/main.py
//! TypeError: Agent.__init__() missing 1 required positional argument: 'client'
//! ```
//!
//! This module closes the gap by shelling to the project's
//! `.venv/bin/python` (or a fallback `python3` on PATH) and asking
//! `inspect.signature` for the real parameter list of every public
//! class / free function in the imported module. The result is
//! converted into the same [`ModuleShapes`] snapshot that
//! `tyc-db::build_external_shapes` already consumes for cross-module
//! shape lookup, so the standard constructor / free-function arity
//! check picks it up with no special-case path in the checker.
//!
//! The cache only ever introspects modules whose **top-level package**
//! is listed in `[dependencies]` / `[dev-dependencies]` (or maps to a
//! declared distribution via a `.dist-info/top_level.txt`). Stdlib
//! and project modules are left untouched — they're resolved through
//! their existing channels (stdlib stubs in `tyc-lsp`, project
//! `.ty`/`.dty` files in `collect_project_shapes`).
//!
//! This is a shared crate consumed by **both** entry points so third-party
//! checks behave identically on the CLI and in the editor:
//!
//! - The `tyc` binary (`tyc check` / `tyc build`) calls the one-shot
//!   [`enrich_project_shapes_with_venv`], which builds a throwaway
//!   [`VenvSignatures`] cache, folds recovered shapes into the project
//!   registry, and reports any un-introspectable declared dependencies.
//! - `tyc-lsp` holds a persistent [`VenvSignatures`] per project root and
//!   calls [`VenvSignatures::enrich_into`] on each check, so wrong-typed /
//!   wrong-arity third-party calls surface as live editor diagnostics. The
//!   cache reuses per-module results across keystrokes and invalidates on a
//!   `.venv/pyvenv.cfg` mtime change (a `uv sync`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;

use tyc_types::{ArityInfo, InterfaceShape, MethodSig, ModuleShapes, Type};

/// One parameter recovered from `inspect.signature`.
#[derive(Debug, Clone, Deserialize)]
struct IntrospectedParam {
    name: String,
    /// One of `"positional_only"`, `"positional_or_keyword"`,
    /// `"var_positional"` (i.e. `*args`), `"keyword_only"`,
    /// `"var_keyword"` (i.e. `**kwargs`).
    kind: String,
    has_default: bool,
    /// `true` when the parameter's default value is literally `None`. This is
    /// the ubiquitous "implicit Optional" idiom — `def f(x: int = None)` /
    /// `Cls(x: str = None)` — where the annotation is a bare scalar but `None`
    /// is in fact a valid argument. Without it the annotation "lies" and a
    /// call passing `None` (or relying on the sentinel) would false-positive
    /// with `tyc::type_mismatch`. When set, [`annotation_to_type`]'s result is
    /// widened to nullable so the check stays sound. Defaults to `false` for
    /// older cached payloads that predate the field.
    #[serde(default)]
    default_is_none: bool,
    /// The parameter's annotation rendered to a string (`"int"`, `"str"`,
    /// `"Optional[int]"`, `"<class 'requests.Session'>"`, …) or `None` when
    /// unannotated. Mapped to a Typhon [`Type`] by [`annotation_to_type`];
    /// anything not confidently recognised degrades to [`Type::Unknown`] so
    /// the checker never false-positives on a shape it can't model yet.
    #[serde(default)]
    annotation: Option<String>,
}

/// One public symbol of an introspected module — what `dir(module)`
/// surfaces, restricted to public names.
#[derive(Debug, Clone, Deserialize)]
struct IntrospectedMember {
    name: String,
    /// One of `"class"`, `"function"`, `"module"`, `"value"`.
    kind: String,
    /// `None` when `inspect.signature` raised (C extensions, built-in
    /// types with no Python signature). We deliberately skip emitting
    /// shape info for these — a missing signature is less surprising
    /// than a wrong one.
    params: Option<Vec<IntrospectedParam>>,
    /// The return annotation rendered to a string, or `None` when the
    /// function has none. Only meaningful for `"function"` members.
    #[serde(default)]
    returns: Option<String>,
    /// Public methods of a `"class"` member, each with its own
    /// (receiver-stripped) signature. `None`/empty for non-class members
    /// or classes whose methods couldn't be introspected. Lets the checker
    /// arity-check `obj.method(...)` calls against a foreign class — the
    /// constructor (`params`) only covers `Cls(...)`.
    #[serde(default)]
    methods: Option<Vec<IntrospectedMethod>>,
}

/// One public method recovered from an introspected class. The `params`
/// list already has the implicit receiver (`self` / `cls`) stripped on the
/// Python side, so it maps straight onto an [`ArityInfo`] via
/// [`arity_info_from_params`].
#[derive(Debug, Clone, Deserialize)]
struct IntrospectedMethod {
    name: String,
    /// `"method"` (instance), `"classmethod"`, or `"staticmethod"`.
    #[serde(default)]
    kind: String,
    params: Vec<IntrospectedParam>,
    #[serde(default)]
    returns: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct IntrospectedModule {
    members: Vec<IntrospectedMember>,
}

/// Per-project introspection cache. Keyed by dotted module name; the
/// value is `Some(shapes)` on success or `None` to record that we
/// already tried and the module either failed to import or yielded
/// nothing usable.
pub struct VenvSignatures {
    /// Path to the Python binary to invoke. `None` after we looked
    /// and found nothing — in that case [`Self::module_shapes`] is a
    /// no-op forever.
    python_bin: Option<PathBuf>,
    /// The directory containing the project's `typhon.toml`. Used to
    /// re-discover the venv interpreter and re-stat `pyvenv.cfg` — it is
    /// deliberately **not** the introspection subprocess's working
    /// directory. For a stdin script (`python -`), `sys.path[0]` is the
    /// process cwd, so running in the project root let an
    /// attacker-controlled `<root>/json.py` shadow the stdlib module the
    /// embedded helper itself imports — exactly the import-shadowing
    /// attack `SECURITY.md` rules out. The subprocess runs in [`Self::scratch`]
    /// instead; not inheriting the parent's cwd also keeps `tyc check`
    /// reproducible regardless of which subdirectory it was run from.
    project_root: PathBuf,
    /// Empty, process-private working directory for the introspection
    /// subprocess (see [`ScratchDir`]). `None` when no private directory
    /// could be created — introspection is then disabled rather than run
    /// in a shared, shadowable directory.
    scratch: Option<ScratchDir>,
    /// Top-level package names that may be introspected. Modules
    /// whose first dotted component isn't here are skipped — this
    /// keeps `import os` from triggering a Python subprocess.
    allowed_top_level: HashSet<String>,
    /// Module name → result. We keep the failure case (`None`) so a
    /// repeated lookup doesn't keep re-spawning Python for the same
    /// broken module.
    cache: HashMap<String, Option<ModuleShapes>>,
    /// `.venv/pyvenv.cfg` mtime captured when the cache was last (re)built.
    /// Used to invalidate the whole cache when the venv changes underneath a
    /// long-lived holder (e.g. the LSP across `uv sync`) — the one-shot CLI
    /// callers never observe a change mid-run, so this is a no-op for them.
    venv_stamp: Option<std::time::SystemTime>,
}

impl VenvSignatures {
    /// Discover the venv's Python (or `python3` on PATH) and build an
    /// empty cache. `allowed_top_level` is the set of import names
    /// the caller is willing to introspect — typically the project's
    /// declared dependencies' top-level packages.
    pub fn for_project_root(project_root: &Path, allowed_top_level: HashSet<String>) -> Self {
        let python_bin = discover_python(project_root);
        Self {
            python_bin,
            project_root: project_root.to_path_buf(),
            scratch: ScratchDir::new(),
            allowed_top_level,
            cache: HashMap::new(),
            venv_stamp: stat_pyvenv_cfg(project_root),
        }
    }

    /// Replace the allow-list (the project's declared dependencies may have
    /// changed since this cache was created — e.g. the user edited
    /// `typhon.toml`). Cheap; leaves the per-module cache intact since a
    /// wider/narrower allow-list only gates *which* modules are fetched.
    pub fn set_allowed_top_level(&mut self, allowed: HashSet<String>) {
        self.allowed_top_level = allowed;
    }

    /// Re-stat `.venv/pyvenv.cfg`; if it changed since the cache was built
    /// (a `uv sync` / venv recreate), drop every cached result and
    /// re-discover the Python binary so the next lookup reflects the new
    /// environment. Mirrors the completion-introspection cache's policy.
    fn refresh_if_venv_changed(&mut self) {
        let current = stat_pyvenv_cfg(&self.project_root);
        if current != self.venv_stamp {
            self.cache.clear();
            self.python_bin = discover_python(&self.project_root);
            self.venv_stamp = current;
        }
    }

    /// Introspect every allow-listed module in `dotted_names` in a
    /// single Python subprocess and populate the cache with the
    /// results. Modules already in the cache are skipped; modules
    /// whose top-level package isn't in the allow-list are skipped
    /// without contacting Python. Duplicate names in `dotted_names`
    /// are deduplicated so a caller that passed the same module
    /// twice doesn't widen the subprocess argument list pointlessly.
    ///
    /// Batching matters: a project with 10 imported dependency
    /// modules used to pay a fresh Python startup (~80–150 ms each)
    /// per module. Doing them in one process drops the per-module
    /// cost to roughly the import itself, and Python's `sys.modules`
    /// cache means sub-packages of the same root (`requests.adapters`,
    /// `requests.sessions`, …) only pay the import cost once.
    ///
    /// On batch failure (timeout, non-zero exit, malformed stdout)
    /// the method falls back to introspecting each module in its
    /// own subprocess. One pathological import — e.g. a package
    /// whose `__init__.py` deadlocks — would otherwise poison every
    /// unrelated module in the batch by recording them all as
    /// misses, regressing checker accuracy versus the prior
    /// per-module flow. (See codex review of PR #98.)
    pub fn preload(&mut self, dotted_names: &[String]) {
        self.refresh_if_venv_changed();
        let Some(python) = self.python_bin.as_ref() else {
            return;
        };
        // No private scratch directory — refuse to run the subprocess in a
        // shared (shadowable) directory. The modules stay uncached, so they
        // surface through the `unintrospectable-dependency` warning instead
        // of silently passing unchecked.
        let Some(scratch) = self.scratch.as_ref() else {
            return;
        };
        let mut seen: HashSet<&str> = HashSet::new();
        let mut to_fetch: Vec<String> = Vec::with_capacity(dotted_names.len());
        for d in dotted_names {
            let top = d.split('.').next().unwrap_or(d.as_str());
            if !self.allowed_top_level.contains(top) {
                continue;
            }
            if self.cache.contains_key(d.as_str()) {
                continue;
            }
            if seen.insert(d.as_str()) {
                to_fetch.push(d.clone());
            }
        }
        if to_fetch.is_empty() {
            return;
        }
        match introspect_batch_via_python(python, scratch.path(), &to_fetch) {
            Some(mut results) => {
                // Consume `to_fetch` so the cache moves the owned
                // module names rather than re-cloning each one, and
                // pull each entry out of `results` with `remove`
                // (single hash probe, no clone). (Gemini review #2.)
                for name in to_fetch {
                    let shapes = results
                        .remove(name.as_str())
                        .map(|intro| shapes_from_introspected(&intro));
                    self.cache.insert(name, shapes);
                }
            }
            None => {
                // Batch round-trip failed (timeout, non-zero exit,
                // malformed JSON). Retry each module on its own so
                // an unrelated heavyweight import doesn't take the
                // whole project down with it.
                for name in to_fetch {
                    let shapes = introspect_batch_via_python(
                        python,
                        scratch.path(),
                        std::slice::from_ref(&name),
                    )
                    .and_then(|mut r| r.remove(name.as_str()))
                    .map(|intro| shapes_from_introspected(&intro));
                    self.cache.insert(name, shapes);
                }
            }
        }
    }

    /// Look up the shapes for a module. Returns `None` when:
    /// - the module's top-level package isn't in the allow-list, or
    /// - no Python is available, or
    /// - the module failed to import / yielded nothing.
    pub fn module_shapes(&mut self, dotted: &str) -> Option<&ModuleShapes> {
        let top = dotted.split('.').next().unwrap_or(dotted);
        if !self.allowed_top_level.contains(top) {
            return None;
        }
        if !self.cache.contains_key(dotted) {
            // Fall through to the batched API with a single module so
            // the introspection path stays uniform.
            self.preload(std::slice::from_ref(&dotted.to_owned()));
        }
        self.cache.get(dotted).and_then(|opt| opt.as_ref())
    }
}

/// Resolve `python3` on PATH for tests that need to skip when no
/// Python is available. Mirrors [`which_python3`] but is callable
/// from sibling crates' tests.
pub fn which_python3_for_test() -> Option<PathBuf> {
    which_python3()
}

/// The Python a project's introspection should use: prefer the project's
/// own venv interpreter, falling back to a `python3` (or `python`) on PATH.
///
/// The venv interpreter lives at `.venv/bin/python` on Unix and
/// `.venv\Scripts\python.exe` on Windows; both are probed so third-party
/// introspection and editor completion work on every shipped platform.
///
/// This is the **only** supported way to obtain an interpreter for
/// introspection. It is public so `tyc-lsp` shares it rather than keeping a
/// second discovery path — a duplicate that, before this was hoisted, did not
/// honour `TYC_NO_INTROSPECT` and so executed dependency import-time code past
/// a kill-switch `SECURITY.md` documents as disabling exactly that.
pub fn discover_python(project_root: &Path) -> Option<PathBuf> {
    // Opt-out kill-switch: introspection imports the project's declared
    // dependencies in a subprocess to recover their type signatures, which
    // executes those packages' import-time code. On an untrusted project a
    // user can disable it entirely (introspection then silently no-ops, as it
    // does when no interpreter is found) by setting `TYC_NO_INTROSPECT`.
    if std::env::var_os("TYC_NO_INTROSPECT").is_some() {
        return None;
    }
    let venv = project_root.join(".venv");
    // Probe the current platform's native layout first. On WSL / Docker mounts
    // / shared folders both layouts can be present, and picking the foreign
    // one yields an interpreter that won't run.
    let unix = venv.join("bin").join("python");
    let windows = venv.join("Scripts").join("python.exe");
    let candidates = if cfg!(windows) {
        [windows, unix]
    } else {
        [unix, windows]
    };
    for candidate in candidates {
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    which_python3()
}

/// mtime of `.venv/pyvenv.cfg` — the file `uv`/`venv` writes when
/// materialising an environment. Its mtime is the cleanest single signal
/// that the venv changed (sync, recreate, package add/remove).
fn stat_pyvenv_cfg(project_root: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(project_root.join(".venv").join("pyvenv.cfg"))
        .and_then(|m| m.modified())
        .ok()
}

/// Resolve `python3` on the user's PATH. Returns `None` when nothing
/// callable named `python3` is reachable — in that case introspection
/// becomes a silent no-op.
fn which_python3() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    // `python3` first (Unix convention), then `python` / `python.exe`, then the
    // Windows `py` launcher — covering Windows and minimal installs that ship
    // only the unsuffixed name.
    let names: [&str; 5] = ["python3", "python", "python.exe", "py.exe", "py"];
    for dir in std::env::split_paths(&path_var) {
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// An empty, process-private working directory for introspection
/// subprocesses.
///
/// For a script fed over stdin (`python -`), CPython puts the process's
/// current directory at `sys.path[0]`, searched **before** the stdlib.
/// Running the subprocess in the project root therefore let an
/// attacker-controlled file named after a stdlib module (`<root>/json.py`)
/// execute in place of the real one the moment the embedded helper ran its
/// own `import json` — the import-shadowing attack `SECURITY.md` rules out.
/// A *fixed* path under the system temp directory is no better on a
/// multi-user host: `/tmp` is world-writable, so another local user can
/// pre-create the directory and plant the same shadowing file.
///
/// `ScratchDir` creates a fresh directory with an unpredictable name via
/// `create_dir` — which, unlike `create_dir_all`, fails on a pre-existing
/// path, so a directory another user pre-created is never adopted. The
/// directory is therefore always owned by this process's user and empty at
/// creation; on Unix it is additionally restricted to mode `0o700`. It is
/// removed again on drop.
///
/// Shared by [`VenvSignatures`] (CLI + LSP diagnostics introspection) and
/// `tyc-lsp`'s completion introspection cache, so both subprocess surfaces
/// enforce the same boundary.
pub struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    /// Create a fresh scratch directory. Returns `None` when none could be
    /// created (read-only temp dir, exhausted disk, pathological name
    /// collisions) — callers must treat that as "introspection unavailable"
    /// rather than fall back to a shared directory.
    pub fn new() -> Option<Self> {
        let base = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        for attempt in 0u32..16 {
            let path = base.join(format!(
                "tyc-introspect-{}-{}-{}",
                std::process::id(),
                nanos,
                attempt
            ));
            // Two properties matter here, and both are load-bearing for the
            // code-execution vector this directory exists to close.
            //
            // 1. `create_dir`, NOT `create_dir_all`: it fails with
            //    `AlreadyExists` on a pre-existing path, so we never adopt a
            //    directory we did not create ourselves.
            // 2. The 0700 mode is passed to `mkdir(2)` itself rather than
            //    applied afterwards with `set_permissions`. A create-then-
            //    chmod sequence leaves a window in which the directory
            //    carries umask-derived permissions — group- or world-writable
            //    under a permissive umask (002 / 000) — and a local attacker
            //    watching the shared temp dir can drop a `json.py` into it
            //    that the introspection subprocess then imports as code.
            //    `mkdir` masks the requested mode with the umask, and 0700
            //    has no group/other bits for the umask to add back, so the
            //    result can never be more permissive than intended.
            //    (PR #360 review, Codex P2.)
            let created = {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    std::fs::DirBuilder::new().mode(0o700).create(&path).is_ok()
                }
                #[cfg(not(unix))]
                {
                    std::fs::create_dir(&path).is_ok()
                }
            };
            if created {
                // Fail closed rather than trust the mode we asked for: if the
                // directory is not private, refuse it (and clean it up) so a
                // caller never runs a subprocess in a world-writable cwd. The
                // old code discarded the chmod result entirely, so a failure
                // silently left a permissive directory in service.
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let private = std::fs::metadata(&path)
                        .map(|m| m.permissions().mode() & 0o077 == 0)
                        .unwrap_or(false);
                    if !private {
                        let _ = std::fs::remove_dir_all(&path);
                        return None;
                    }
                }
                return Some(Self { path });
            }
        }
        None
    }

    /// The scratch directory's path — guaranteed created, owned by this
    /// process's user, and empty at creation time.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Embedded Python helper. Reads one or more dotted module names from
/// `sys.argv[1:]`, imports each, and prints a single JSON object
/// mapping `{module_name: {"members": [...]}}` to stdout. Per-module
/// failures (`ImportError`, `AttributeError`, …) yield an empty
/// `{"members": []}` entry so the Rust side records a clean miss
/// rather than surfacing subprocess noise.
///
/// Batching is a deliberate optimisation: a fresh Python subprocess
/// costs ~80–150 ms on most systems before any user code runs, and
/// the type checker had previously paid that per imported third-party
/// module. Importing every requested module in one process drops the
/// per-module overhead to roughly the package's own import time;
/// `sys.modules` then dedupes sub-packages of the same root.
///
/// Intentionally stdlib-only so it works against any Python on hand.
const INTROSPECT_SCRIPT: &str = r#"
import sys, json, importlib, inspect

PARAM_KIND_MAP = {
    inspect.Parameter.POSITIONAL_ONLY: "positional_only",
    inspect.Parameter.POSITIONAL_OR_KEYWORD: "positional_or_keyword",
    inspect.Parameter.VAR_POSITIONAL: "var_positional",
    inspect.Parameter.KEYWORD_ONLY: "keyword_only",
    inspect.Parameter.VAR_KEYWORD: "var_keyword",
}

def kind_of(obj):
    if inspect.isclass(obj):
        return "class"
    if inspect.ismodule(obj):
        return "module"
    if callable(obj):
        return "function"
    return "value"

def ann_to_str(ann):
    # Render an annotation to a stable string the Rust side can map to a
    # Typhon type. Type objects -> their bare name ("int", "Session");
    # stringised annotations (PEP 563 `from __future__ import annotations`)
    # pass through as-is; everything else falls back to `str()`. Anything
    # the Rust mapper doesn't recognise degrades to `Unknown`, so being
    # approximate here is safe.
    if ann is inspect.Parameter.empty or ann is None:
        return None
    if isinstance(ann, type):
        return getattr(ann, "__name__", None) or str(ann)
    if isinstance(ann, str):
        return ann
    return str(ann)

def params_of(obj):
    try:
        sig = inspect.signature(obj)
    except Exception:
        # `inspect.signature` is documented to raise TypeError / ValueError
        # when an object has no recoverable signature, but third-party
        # objects raise plenty else. A werkzeug `LocalProxy` re-exported at
        # module scope (`flask.current_app` / `g` / `request` / `session`)
        # is `callable()`, so `kind_of` labels it a "function" and we probe
        # its signature — which raises `RuntimeError: Working outside of
        # application context`. Catching only (TypeError, ValueError) let
        # that propagate and crash the *entire* module's introspection,
        # silently disabling every third-party check for the library (Flask
        # constructors/functions all went unchecked). Treat ANY failure as
        # "no signature recoverable" → the member is skipped (stays lenient).
        return None
    out = []
    for p in sig.parameters.values():
        out.append({
            "name": p.name,
            "kind": PARAM_KIND_MAP.get(p.kind, "positional_or_keyword"),
            "has_default": p.default is not inspect.Parameter.empty,
            # The "implicit Optional" idiom (`x: int = None`): the annotation
            # is a bare scalar but None is a valid argument. The Rust side
            # widens the param type to nullable so a `None` argument doesn't
            # false-positive.
            "default_is_none": p.default is None,
            "annotation": ann_to_str(p.annotation),
        })
    return out

def returns_of(obj):
    try:
        sig = inspect.signature(obj)
    except Exception:
        # See `params_of` — any signature failure means "no return type
        # recoverable", never a crash that takes the whole module down.
        return None
    return ann_to_str(sig.return_annotation)

def methods_of(cls):
    # Public methods of `cls`, each with its receiver-stripped signature.
    # Only plain Python functions / classmethods / staticmethods are
    # captured — C-extension slots and descriptors raise from
    # `inspect.signature` (or aren't functions) and are skipped, so a
    # numpy ndarray's C methods stay unchecked (lenient) rather than
    # false-positive. Properties carry no call arity and are skipped.
    out = []
    try:
        names = dir(cls)
    except BaseException:
        return out
    for mname in names:
        if mname.startswith("_"):
            continue
        try:
            raw = inspect.getattr_static(cls, mname)
        except BaseException:
            continue
        if isinstance(raw, staticmethod):
            mkind, strip = "staticmethod", False
            target = raw.__func__
        elif isinstance(raw, classmethod):
            mkind, strip = "classmethod", True
            target = raw.__func__
        elif inspect.isfunction(raw):
            mkind, strip = "method", True
            target = raw
        else:
            # property, slot wrapper, C method_descriptor, plain attribute…
            continue
        p = params_of(target)
        if p is None:
            continue
        # Drop the leading receiver (self / cls) ONLY when it's a genuine
        # leading positional. A decorator-wrapped method whose signature is
        # `(*args, **kwargs)` (common in sklearn via functools.wraps) has no
        # real receiver slot — stripping its `*args` would wrongly cap the
        # method's positional arity and false-positive every valid call.
        if strip and p and p[0]["kind"] in ("positional_only", "positional_or_keyword"):
            p = p[1:]
        out.append({"name": mname, "kind": mkind, "params": p, "returns": returns_of(target)})
    return out

def introspect_one(mod_name):
    try:
        m = importlib.import_module(mod_name)
    except BaseException:
        return {"members": []}
    members = []
    for name in dir(m):
        if name.startswith("_"):
            continue
        try:
            obj = getattr(m, name)
        except BaseException:
            continue
        try:
            kind = kind_of(obj)
            members.append({
                "name": name,
                "kind": kind,
                "params": params_of(obj) if kind in ("class", "function") else None,
                "returns": returns_of(obj) if kind == "function" else None,
                "methods": methods_of(obj) if kind == "class" else None,
            })
        except Exception:
            # Defense in depth: never let one pathological member crash the
            # whole module's introspection. `kind_of`'s `callable(obj)` can
            # raise on an exotic descriptor, and `methods_of` walks `dir(cls)`
            # on a class whose metaclass misbehaves. A single bad member must
            # only lose itself, not every other class/function in the module.
            # `Exception` (not `BaseException`) so a genuine `KeyboardInterrupt`
            # / `SystemExit` still terminates the subprocess — every realistic
            # crash-causer here (the werkzeug `LocalProxy` `RuntimeError`, a
            # Django `ImproperlyConfigured`) is an `Exception` subclass.
            continue
    return {"members": members}

def main():
    result = {}
    for mod_name in sys.argv[1:]:
        result[mod_name] = introspect_one(mod_name)
    print(json.dumps(result))

main()
"#;

/// Shell to Python with [`INTROSPECT_SCRIPT`] and introspect every
/// module in `modules` in a single subprocess. Returns the parsed
/// `{module_name: IntrospectedModule}` map on success.
///
/// Bounded by a per-module timeout budget (5 s baseline + 1 s per
/// module in the batch) so a package whose import-time side-effects
/// hang doesn't wedge `tyc check` regardless of how many modules
/// are in the batch.
fn introspect_batch_via_python(
    python: &Path,
    cwd: &Path,
    modules: &[String],
) -> Option<HashMap<String, IntrospectedModule>> {
    use std::io::{Read, Write};
    if modules.is_empty() {
        return Some(HashMap::new());
    }
    let mut cmd = Command::new(python);
    cmd.arg("-");
    for m in modules {
        cmd.arg(m);
    }
    cmd.current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    // Retry transient spawn failures once. fork() can fail with EAGAIN
    // under CI fork pressure (parallel tests, container PID limits),
    // and a silent None here would cache the module as a miss for the
    // rest of the run. A 50 ms backoff is plenty to clear typical
    // resource contention.
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => {
            std::thread::sleep(Duration::from_millis(50));
            cmd.spawn().ok()?
        }
    };
    {
        let mut stdin = child.stdin.take()?;
        stdin.write_all(INTROSPECT_SCRIPT.as_bytes()).ok()?;
    }
    // Drain stdout on a dedicated thread — large modules (e.g.
    // `numpy`) overflow the pipe buffer and deadlock the child if we
    // only read after `wait()`. The batched output is also larger
    // than a single-module response, so the dedicated reader matters
    // more here.
    let stdout = child.stdout.take()?;
    let drainer = std::thread::spawn(move || -> Vec<u8> {
        // Cap the read: a hostile (or merely chatty) dependency that writes to
        // stdout at pipe speed for the whole timeout window would otherwise
        // fill RAM. Introspection output is a few KB of JSON; 32 MiB is vast
        // headroom yet bounds the worst case. An over-cap response simply fails
        // to parse downstream and is treated as a miss.
        const CAP: u64 = 32 * 1024 * 1024;
        let mut buf = Vec::with_capacity(64 * 1024);
        let _ = std::io::Read::take(stdout, CAP).read_to_end(&mut buf);
        buf
    });
    // 5 s startup baseline + 1 s per module in the batch. Generous
    // enough that even a ten-module batch with a slow
    // `pydantic`-style import has headroom; tight enough that a
    // wedged interpreter still bails before `tyc check` looks
    // frozen.
    let timeout = Duration::from_secs(5 + modules.len() as u64);
    let deadline = std::time::Instant::now() + timeout;
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
    let bytes = drainer.join().ok()?;
    if !success {
        return None;
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    serde_json::from_str(text.trim()).ok()
}

/// Convert an introspection result into the [`ModuleShapes`] form the
/// type checker already consumes. Classes become [`InterfaceShape`]s
/// with each `__init__` parameter modelled as a field (so the
/// constructor-arity path works untouched); free functions become
/// [`ArityInfo`] entries.
///
/// Classes / functions whose signature couldn't be recovered, or
/// whose `__init__` declares `**kwargs`, are skipped — better to miss
/// the check than to surface a false positive on a permissive Python
/// API. `*args` is treated similarly for class constructors. Free
/// functions with `*args` populate the existing `max_positional =
/// None` / variadic shape and stay checkable for keyword args.
fn shapes_from_introspected(intro: &IntrospectedModule) -> ModuleShapes {
    let mut class_shapes: HashMap<String, InterfaceShape> = HashMap::new();
    let mut function_arities: HashMap<String, ArityInfo> = HashMap::new();
    for member in &intro.members {
        let Some(params) = &member.params else {
            continue;
        };
        match member.kind.as_str() {
            "class" => {
                if let Some(mut shape) = class_shape_from_params(params) {
                    // Attach introspected method signatures so `obj.method(...)`
                    // calls are arity-checked (the constructor `params` only
                    // cover `Cls(...)`). Methods we couldn't introspect simply
                    // aren't present, and the shape stays `partial`, so missing
                    // ones remain lenient — no false `attribute_not_found`.
                    if let Some(methods) = &member.methods {
                        for m in methods {
                            if let Some(sig) = method_sig_from_introspected(m) {
                                shape.methods.insert(m.name.clone(), sig);
                            }
                        }
                    }
                    class_shapes.insert(member.name.clone(), shape);
                }
            }
            "function" => {
                if let Some(info) = arity_info_from_params(params, member.returns.as_deref()) {
                    function_arities.insert(member.name.clone(), info);
                }
            }
            _ => {}
        }
    }
    ModuleShapes {
        class_shapes,
        class_type_params: HashMap::new(),
        function_arities,
        sealed_unions: HashMap::new(),
        interfaces: HashMap::new(),
        // Introspected Python modules carry no Typhon newtypes, type
        // aliases, enums, or `frozen`-modifier classes.
        newtypes: HashMap::new(),
        type_aliases: HashMap::new(),
        enums: HashMap::new(),
        frozen_classes: std::collections::HashSet::new(),
        gatherable_async_fns: std::collections::HashSet::new(),
        // Introspected Python modules carry no inferred-variance or
        // higher-kinded metadata — those are Typhon-source concepts.
        class_param_variance: HashMap::new(),
        hkt_param_names: std::collections::HashSet::new(),
    }
}

/// Build an [`InterfaceShape`] for an introspected class. `params`
/// already has `self` stripped (Python's `inspect.signature(Cls)`
/// returns the constructor signature minus the receiver).
///
/// Returns `None` when the constructor accepts `**kwargs` or `*args`
/// — those forms can't be matched against the rigid
/// "field_order + field_defaults" shape used by the constructor-arity
/// check without surfacing a false positive on every extra kwarg.
fn class_shape_from_params(params: &[IntrospectedParam]) -> Option<InterfaceShape> {
    let mut field_order: Vec<String> = Vec::new();
    let mut field_defaults: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut fields: HashMap<String, Type> = HashMap::new();
    for p in params {
        match p.kind.as_str() {
            // `**kwargs` / `*args` defeat the rigid field-based
            // constructor check — bail out and let the call go
            // through unchecked. Keeping the constructor untyped is
            // strictly safer than emitting false positives on every
            // extra kwarg.
            "var_keyword" | "var_positional" => return None,
            _ => {}
        }
        if p.has_default {
            field_defaults.insert(p.name.clone());
        }
        field_order.push(p.name.clone());
        fields.insert(p.name.clone(), param_type_from(p));
    }
    if field_order.is_empty() {
        // A zero-arg constructor still benefits from being modelled —
        // calls like `Foo(extra="bad")` would otherwise pass.
        // Returning the empty shape is fine; the unknown-kwarg loop
        // handles the "extra kwarg on no-arg class" case.
    }
    Some(InterfaceShape {
        methods: HashMap::new(),
        fields,
        field_order,
        field_defaults,
        // Introspected venv classes don't carry inheritance info here;
        // the constructor check falls back to direct fields, which is
        // the pre-effective-shape behaviour. Foreign-class inheritance
        // chains land via the typeshed stub path.
        bases: Vec::new(),
        // v0.8.0 carry-over: mark the shape partial so the v0.8.0
        // `tyc::attribute_not_found` diagnostic stays lenient on
        // attribute access against venv-introspected classes —
        // `inspect.signature(Cls)` reflects the constructor but not
        // the method surface, so we can't soundly claim a method is
        // missing.
        partial: true,
    })
}

/// Map an introspected annotation string to a Typhon [`Type`].
///
/// Conservative by design — only forms we can model precisely are
/// recognised:
/// - scalar built-ins (`int` / `str` / `bool` / `float` / `bytes` / `None`);
/// - the nullable forms `Optional[X]` and the 2-member `X | None`;
/// - small non-nullable unions: the 2-member `X | Y` / `Union[X, Y]` forms
///   become a real `Type::Union`, but ONLY when both members resolve to a
///   concrete type (any unresolvable member degrades the whole union to
///   `Unknown`). A2.
/// - fully-concrete parametric containers `list[X]` / `set[X]` /
///   `frozenset[X]` / `dict[K, V]` and fixed-arity `tuple[T1, …]`
///   (recursively — a container whose element doesn't resolve degrades the
///   whole thing).
///
/// Everything else — 3+-member unions, `tuple[T, ...]`, `Callable`,
/// foreign classes, unknown typing constructs — degrades to
/// [`Type::Unknown`] (which accepts anything). So annotation capture can
/// only ever *add* true positives; it never rejects valid code on a shape
/// we can't represent.
fn annotation_to_type(ann: &str) -> Type {
    let mut s = ann.trim();
    // Unwrap the `<class 'X'>` form that `str(type_object)` produces.
    if let Some(inner) = s
        .strip_prefix("<class '")
        .and_then(|r| r.strip_suffix("'>"))
    {
        s = inner;
    }
    s = s.trim();

    // `Annotated[T, meta…]` (optionally `typing.` / `typing_extensions.`
    // -qualified) → resolve to the FIRST type argument `T`, discarding the
    // metadata. This is the form FastAPI / Typer / Pydantic stamp onto their
    // parameters (`Annotated[str, FieldInfo(...)]`); without this the whole
    // annotation degraded to `Unknown` and wrong-typed kwargs went uncaught.
    // The metadata args are arbitrary `repr()` text (commas / brackets /
    // parens), so we take only the first top-level comma-separated segment;
    // if `T` itself doesn't resolve we degrade to `Unknown` exactly as before
    // (never stricter than today). A1.
    {
        let head_stripped = s
            .strip_prefix("typing.")
            .or_else(|| s.strip_prefix("typing_extensions."))
            .unwrap_or(s);
        if let Some(inner) = head_stripped
            .strip_prefix("Annotated[")
            .and_then(|r| r.strip_suffix(']'))
        {
            let parts = split_top_level_commas(inner);
            if let Some(first) = parts.first() {
                return annotation_to_type(first.trim());
            }
            return Type::Unknown;
        }
    }

    // `Optional[X]` (optionally `typing.`-qualified) → `X | None`.
    let unqualified = s.strip_prefix("typing.").unwrap_or(s);
    if let Some(inner) = unqualified
        .strip_prefix("Optional[")
        .and_then(|r| r.strip_suffix(']'))
    {
        return nullable_of(annotation_to_type(inner));
    }

    // `Union[A, B, …]` (optionally `typing.`-qualified) — the bracketed PEP 484
    // form. Mapped through the same small-union machinery as the `|` form
    // (A2): a 2-member non-nullable union becomes a real `Type::Union`; the
    // nullable 2-member form (`Union[X, None]`) becomes `X | None`; everything
    // wider or with an unresolvable member degrades to `Unknown`.
    if let Some(inner) = unqualified
        .strip_prefix("Union[")
        .and_then(|r| r.strip_suffix(']'))
    {
        let members = split_top_level_commas(inner);
        return union_from_members(&members);
    }

    // `X | Y` / `X | None` (PEP 604). Splits on EVERY top-level pipe so we know
    // the true arity (`split_top_level_union` only finds the first pipe, which
    // would mis-read `int | str | None` as a 2-member union). `union_from_members`
    // applies the soundness guard: a 2-member union (nullable or not) is modelled
    // precisely; 3+ members or an unresolvable member degrade to `Unknown`. A2.
    if s.contains('|') {
        let members = split_top_level_pipes(s);
        if members.len() >= 2 {
            return union_from_members(&members);
        }
    }

    // Parametric containers: `list[X]` / `set[X]` / `frozenset[X]` /
    // `dict[K, V]` (and their `typing.`-qualified or capitalised aliases).
    // Inner types map recursively; if ANY inner doesn't resolve to a concrete
    // type the whole container degrades to `Unknown` (permissive), so we only
    // ever emit a fully-known container shape — never `list[Unknown]`. The
    // checker does bidirectional element widening (`[1, 2]` into `list[float]`
    // is fine), so a concrete container shape adds true positives only.
    if let Some((head, inner)) = split_generic(s) {
        let head = head.rsplit('.').next().unwrap_or(head);
        // Single-element (`list`/`set`/`frozenset`), two-element (`dict`), and
        // fixed-arity `tuple` containers map element-wise. A fully-mapped
        // `Generic(head, [..])` matches what the checker builds from the same
        // annotation, so it integrates with the existing assignability rules.
        let arity_ok: Option<(&str, bool)> = match head {
            "list" | "List" => Some(("list", true)),
            "set" | "Set" => Some(("set", true)),
            "frozenset" | "FrozenSet" => Some(("frozenset", true)),
            "dict" | "Dict" => Some(("dict", false)),
            "tuple" | "Tuple" => Some(("tuple", false)),
            _ => None,
        };
        if let Some((name, single)) = arity_ok {
            let parts = split_top_level_commas(inner);
            // Homogeneous `tuple[X, ...]` (Ellipsis) and degenerate shapes
            // stay permissive — only fixed-arity element lists are mapped.
            let shape_ok = match name {
                "dict" => parts.len() == 2,
                "tuple" => !parts.is_empty() && !parts.iter().any(|p| p.trim() == "..."),
                _ if single => parts.len() == 1,
                _ => false,
            };
            if shape_ok {
                let mapped: Vec<Type> =
                    parts.iter().map(|p| annotation_to_type(p.trim())).collect();
                if mapped
                    .iter()
                    .any(|t| matches!(t, Type::Unknown | Type::Any))
                {
                    return Type::Unknown;
                }
                return Type::Generic(name.to_owned(), mapped);
            }
        }
        // Foreign generic class, `Callable[...]`, `tuple[X, ...]`, etc. — stay
        // permissive rather than guess.
        return Type::Unknown;
    }

    // Drop a module qualifier on a bare dotted name (`builtins.int`), but
    // leave parametric forms untouched so we don't mangle them.
    let bare = if !s.contains('[') && !s.contains('|') && !s.contains(' ') {
        s.rsplit('.').next().unwrap_or(s)
    } else {
        s
    };
    match bare {
        "int" => Type::Int,
        "str" => Type::Str,
        "bool" => Type::Bool,
        "float" => Type::Float,
        "bytes" => Type::Bytes,
        "None" | "NoneType" => Type::None,
        _ => Type::Unknown,
    }
}

/// Split a `Head[inner]` annotation into `(head, inner)` — the text inside the
/// outermost brackets. `None` when `s` isn't of that form.
fn split_generic(s: &str) -> Option<(&str, &str)> {
    let open = s.find('[')?;
    if !s.ends_with(']') {
        return None;
    }
    let head = s[..open].trim();
    if head.is_empty() {
        return None;
    }
    Some((head, &s[open + 1..s.len() - 1]))
}

/// Split `s` on top-level commas (ignoring commas nested inside `[...]` /
/// `(...)`). Used to separate container type arguments.
fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, &b) in s.as_bytes().iter().enumerate() {
        match b {
            b'[' | b'(' => depth += 1,
            b']' | b')' => depth -= 1,
            b',' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// Map an introspected parameter to its Typhon [`Type`], applying the
/// "implicit Optional" widening: a param whose default is literally `None`
/// (`def f(x: int = None)`) accepts `None`, so its bare-scalar / container
/// annotation is widened to nullable. Without this, a call passing `None`
/// (or any value the checker infers as nullable) would false-positive with
/// `tyc::type_mismatch` against a third-party annotation that "lies".
///
/// Only concrete, non-nullable types are widened — an already-`Optional[X]`
/// annotation (`Type::Union`), `Unknown`, or `None` is left untouched (the
/// author already expressed the optionality, or there's nothing to widen).
/// The widening only ever *adds* accepted values, so it can only remove
/// false positives, never introduce one; a genuinely wrong-typed argument
/// (`x="str"` for `x: int = None`) still fails against `int | None`.
fn param_type_from(param: &IntrospectedParam) -> Type {
    let ty = param
        .annotation
        .as_deref()
        .map(annotation_to_type)
        .unwrap_or(Type::Unknown);
    if !param.default_is_none {
        return ty;
    }
    match ty {
        Type::Unknown | Type::Any | Type::None | Type::Union(_) => ty,
        other => nullable_of(other),
    }
}

/// Wrap `inner` as nullable (`inner | None`). A non-mappable inner collapses
/// to `Unknown` (a nullable-`Unknown` is just `Unknown` — still permissive).
fn nullable_of(inner: Type) -> Type {
    match inner {
        Type::Unknown | Type::Any => Type::Unknown,
        Type::None => Type::None,
        other => Type::Union(vec![other, Type::None]),
    }
}

/// Split `s` on every top-level `|` (ignoring pipes nested inside
/// `[...]` / `(...)`), returning all members. Used to recognise PEP 604
/// unions (`X | None`, `X | Y`) with their true arity, so a 3+-member union
/// isn't mistaken for a 2-member one. A single member (no top-level pipe)
/// yields a one-element vec.
fn split_top_level_pipes(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, &b) in s.as_bytes().iter().enumerate() {
        match b {
            b'[' | b'(' => depth += 1,
            b']' | b')' => depth -= 1,
            b'|' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// Map the (already-split) members of a `Union[...]` / `X | Y` annotation to a
/// Typhon [`Type`], applying the A2 soundness guard for small non-nullable
/// unions:
///
/// - The 2-member nullable forms (`X | None`, `Union[X, None]`) collapse to
///   `X | None` via [`nullable_of`] — exactly as before A2.
/// - A 2-member NON-nullable union (`str | bytes`, `Union[int, str]`) becomes a
///   real `Type::Union([A, B])` — *only* when BOTH members resolve to a
///   concrete (non-`Unknown`/`Any`) type. If either member is unresolvable the
///   whole union degrades to `Unknown` (permissive), so we never reject a value
///   on a union we can't fully represent.
/// - 3+-member unions (after deduping) stay `Unknown` for now — modelling them
///   precisely is deferred; degrading is always sound.
///
/// Because `Type::union_of` flattens/dedups, a redundant `int | int` collapses
/// to `Int` and a `X | None | None` to `X | None` — these reduce to the
/// 1-/2-member cases naturally.
fn union_from_members(members: &[&str]) -> Type {
    let mapped: Vec<Type> = members
        .iter()
        .map(|m| annotation_to_type(m.trim()))
        .collect();

    // Partition into the None members and the rest. The nullable forms reuse
    // the existing `nullable_of` machinery; a single non-None concrete member
    // alongside `None` is exactly `X | None`.
    let mut non_none: Vec<Type> = Vec::new();
    let mut has_none = false;
    for t in &mapped {
        match t {
            Type::None => has_none = true,
            other => non_none.push(other.clone()),
        }
    }

    // Dedup the non-None members so `int | int` and `Union[str, str]` reduce.
    let mut unique: Vec<Type> = Vec::new();
    for t in non_none {
        if !unique.contains(&t) {
            unique.push(t);
        }
    }

    match (has_none, unique.len()) {
        // Pure nullable (`None | None`) or nothing concrete — permissive.
        (true, 0) => Type::None,
        // 2-member nullable form `X | None` — model precisely if `X` resolves.
        (true, 1) => nullable_of(unique.into_iter().next().unwrap()),
        // A single distinct non-None member (e.g. a redundant `int | int`)
        // collapses to that member.
        (false, 1) => unique.into_iter().next().unwrap(),
        // 2-member non-nullable union — model precisely, but ONLY when both
        // members are concrete; any unresolvable member ⇒ degrade to Unknown.
        (false, 2) => {
            if unique
                .iter()
                .any(|t| matches!(t, Type::Unknown | Type::Any))
            {
                return Type::Unknown;
            }
            Type::union_of(unique)
        }
        // 3+ distinct members (deferred), or a `X | Y | None` shape — stay
        // permissive rather than guess.
        _ => Type::Unknown,
    }
}

/// Build an [`ArityInfo`] for an introspected free function. `returns` is
/// the function's return annotation (or `None`).
/// Build a [`MethodSig`] for an introspected class method. The `params`
/// already have the receiver stripped (Python side), so they map straight
/// onto an [`ArityInfo`]. Returns `None` only when the signature couldn't be
/// turned into arity info. A `**kwargs` / `*args` method still yields a usable
/// (permissive-max) shape via `arity_info_from_params`, so the *minimum*
/// required positional count is still enforced — `model.fit()` with `fit(self,
/// X, y=None, **kw)` still flags the missing `X`.
fn method_sig_from_introspected(m: &IntrospectedMethod) -> Option<MethodSig> {
    let info = arity_info_from_params(&m.params, m.returns.as_deref())?;
    let param_types = info.param_types.clone();
    let return_type = info.return_type.clone();
    Some(MethodSig {
        arity: info.param_names.len(),
        return_type,
        is_property: false,
        is_static: m.kind == "staticmethod",
        is_classmethod: m.kind == "classmethod",
        arity_info: info,
        param_types,
        is_async: false,
    })
}

fn arity_info_from_params(
    params: &[IntrospectedParam],
    returns: Option<&str>,
) -> Option<ArityInfo> {
    let mut param_names: Vec<String> = Vec::new();
    let mut param_types: Vec<Type> = Vec::new();
    let mut required_positional: Vec<bool> = Vec::new();
    let mut kwonly_names: Vec<String> = Vec::new();
    let mut kwonly_types: Vec<Type> = Vec::new();
    let mut kwonly_required: Vec<String> = Vec::new();
    let mut has_kwarg = false;
    let mut has_vararg = false;
    let mut posonly_count = 0usize;
    for p in params {
        let ty = param_type_from(p);
        match p.kind.as_str() {
            "var_positional" => has_vararg = true,
            "var_keyword" => has_kwarg = true,
            "keyword_only" => {
                kwonly_names.push(p.name.clone());
                kwonly_types.push(ty);
                if !p.has_default {
                    kwonly_required.push(p.name.clone());
                }
            }
            _ => {
                if p.kind == "positional_only" {
                    posonly_count += 1;
                }
                param_names.push(p.name.clone());
                param_types.push(ty);
                required_positional.push(!p.has_default);
            }
        }
    }
    let max_positional = if has_vararg {
        None
    } else {
        Some(param_names.len())
    };
    let min_positional = required_positional.iter().filter(|r| **r).count();
    let return_type = returns.map(annotation_to_type).unwrap_or(Type::Unknown);
    Some(ArityInfo {
        param_names,
        min_positional,
        required_positional,
        max_positional,
        posonly_count,
        kwonly_names,
        kwonly_required,
        has_kwarg,
        vararg_type: None,
        param_types,
        kwonly_types,
        return_type,
    })
}

/// Walk every `.ty` file under `paths` and collect the dotted module
/// names referenced by `import X` / `from X import …` statements.
///
/// Uses a lightweight line-by-line scan rather than a full parse — the
/// shapes-enrichment pass needs to know *which* modules might be
/// imported, not how their members are used. A few false-positive
/// strings inside multi-line constructs (e.g. a triple-quoted string
/// whose lines happen to start with `import`) won't matter:
/// [`VenvSignatures::module_shapes`] gates every lookup on the
/// allow-list, so anything we wouldn't have introspected anyway is
/// silently skipped.
pub fn collect_imported_modules(paths: &[PathBuf]) -> Vec<String> {
    let mut found: HashSet<String> = HashSet::new();
    for root in paths {
        let files = match collect_ty_files_for_scan(root) {
            Some(f) => f,
            None => continue,
        };
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            for line in text.lines() {
                let trimmed = line.trim_start();
                if let Some(rest) = trimmed.strip_prefix("import ") {
                    extract_dotted_modules_from_import(rest, &mut found);
                } else if let Some(rest) = trimmed.strip_prefix("from ") {
                    if let Some(name) = first_dotted_token(rest) {
                        found.insert(name);
                    }
                } else if let Some(rest) = trimmed.strip_prefix("lazy import ") {
                    // `lazy import np = numpy` — the RHS after `=`
                    // (or the bare module path) is what we need to
                    // introspect. Strip the alias prefix when present.
                    let after_eq = rest.split_once('=').map(|(_, r)| r).unwrap_or(rest);
                    if let Some(name) = first_dotted_token(after_eq) {
                        found.insert(name);
                    }
                }
            }
        }
    }
    let mut out: Vec<String> = found.into_iter().collect();
    out.sort();
    out
}

/// Extract the leading dotted-name token from an `import` continuation
/// like `"foo.bar as baz"` or `"foo.bar, qux.zap"`. All modules listed
/// in a comma-separated import are collected.
fn extract_dotted_modules_from_import(rest: &str, out: &mut HashSet<String>) {
    for chunk in rest.split(',') {
        let chunk = chunk.trim();
        let token = chunk.split_whitespace().next().unwrap_or("");
        if !token.is_empty() && is_valid_dotted_name(token) {
            out.insert(token.to_owned());
        }
    }
}

/// Strip the leading dotted-name token off `s` and return it. Returns
/// `None` when the first non-whitespace span isn't a valid dotted
/// identifier (i.e. the line wasn't actually an import).
fn first_dotted_token(s: &str) -> Option<String> {
    let token = s.split_whitespace().next()?;
    if is_valid_dotted_name(token) {
        Some(token.to_owned())
    } else {
        None
    }
}

fn is_valid_dotted_name(s: &str) -> bool {
    !s.is_empty()
        && s.split('.').all(|seg| {
            let mut chars = seg.chars();
            match chars.next() {
                Some(c) if c == '_' || c.is_ascii_alphabetic() => {
                    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
                }
                _ => false,
            }
        })
}

fn collect_ty_files_for_scan(root: &Path) -> Option<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = Vec::new();
    if root.is_file() {
        files.push(root.to_path_buf());
        return Some(files);
    }
    if !root.is_dir() {
        return None;
    }
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("ty") {
                files.push(path);
            }
        }
    }
    Some(files)
}

/// Walk every `.ty` file under `paths`, introspect each third-party
/// module that's imported but not already in `project_shapes`, and
/// fold the resulting shapes into the registry.
///
/// `project_module_set` is the set of dotted module names contributed
/// by the project itself — these are skipped, since their shapes are
/// already populated from `.ty`/`.dty` sources.
///
/// Failures (no venv, no Python, import-time exception) are silent:
/// the worst case is the existing behaviour where the checker has no
/// signature for the module and the runtime catches the missing
/// argument.
pub fn enrich_project_shapes_with_venv(
    paths: &[PathBuf],
    project_root: &Path,
    project_module_set: &HashSet<String>,
    allowed_top_level: HashSet<String>,
    project_shapes: &mut HashMap<String, ModuleShapes>,
) -> Vec<String> {
    if allowed_top_level.is_empty() {
        return Vec::new();
    }
    let mut cache = VenvSignatures::for_project_root(project_root, allowed_top_level);
    cache.enrich_into(paths, project_module_set, project_shapes)
}

impl VenvSignatures {
    /// Fold recovered third-party shapes for every allow-listed module
    /// imported under `paths` into `project_shapes`, reusing this cache's
    /// per-module results. The CLI builds a throwaway cache via
    /// [`enrich_project_shapes_with_venv`]; the LSP holds one across edits so
    /// repeated checks only shell to Python when a genuinely new dependency
    /// module appears (or the venv changed — see [`Self::refresh_if_venv_changed`]).
    /// Returns the top-level declared dependencies that couldn't be
    /// introspected (for the unintrospectable-dependency warning).
    pub fn enrich_into(
        &mut self,
        paths: &[PathBuf],
        project_module_set: &HashSet<String>,
        project_shapes: &mut HashMap<String, ModuleShapes>,
    ) -> Vec<String> {
        if self.allowed_top_level.is_empty() {
            return Vec::new();
        }
        let imports = collect_imported_modules(paths);
        // Only modules whose top-level package is a declared dependency are
        // candidates (the allow-list); stdlib / project modules are excluded.
        let needed: Vec<String> = imports
            .iter()
            .filter(|m| !project_shapes.contains_key(*m) && !project_module_set.contains(*m))
            .filter(|m| self.allowed_top_level.contains(top_level(m)))
            .cloned()
            .collect();
        if needed.is_empty() {
            return Vec::new();
        }

        self.refresh_if_venv_changed();

        // No reachable `.venv`/`python3`: every needed declared-dependency
        // module is unintrospectable. Report each top-level package so the
        // caller can warn that third-party checks were skipped rather than
        // silently passing (the most dangerous failure mode for this feature).
        if self.python_bin.is_none() {
            let mut tops: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for m in &needed {
                tops.insert(top_level(m).to_owned());
            }
            return tops.into_iter().collect();
        }

        self.preload(&needed);
        // Track per-top-level success so a package whose root introspected
        // fine isn't reported just because one submodule (`requests.adapters`)
        // failed.
        let mut ok_tops: HashSet<String> = HashSet::new();
        let mut failed_tops: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for module in needed {
            let top = top_level(&module).to_owned();
            // A module that failed to import yields an *empty* `ModuleShapes`
            // (the embedded Python returns `{"members": []}` on `ImportError`),
            // and a module whose members are all C-extension callables
            // introspects to nothing — both mean "no signatures recovered", so
            // treat an empty result the same as a miss for the warning.
            let recovered = self.module_shapes(&module).cloned();
            match recovered {
                Some(shapes)
                    if !(shapes.class_shapes.is_empty() && shapes.function_arities.is_empty()) =>
                {
                    project_shapes.insert(module, shapes);
                    ok_tops.insert(top);
                }
                _ => {
                    failed_tops.insert(top);
                }
            }
        }
        failed_tops
            .into_iter()
            .filter(|t| !ok_tops.contains(t))
            .collect()
    }
}

/// First dotted component of a module path (`requests.adapters` → `requests`).
fn top_level(module: &str) -> &str {
    module.split('.').next().unwrap_or(module)
}

/// Compute the introspection allow-list — the declared dependencies'
/// top-level import names — from a project's `typhon.toml`. Mirrors the set
/// the CLI builds from `[dependencies]` + `[dev-dependencies]` keys, so the
/// LSP introspects exactly the same modules `tyc check` would. Empty when
/// there's no `typhon.toml` or it declares no dependencies.
pub fn allowed_top_level_from_project(project_root: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
    let Ok(text) = std::fs::read_to_string(project_root.join("typhon.toml")) else {
        return out;
    };
    // `toml::from_str` (the serde path) parses a whole document; with the
    // toml 1.x crate, `str::parse::<toml::Value>()` parses a single TOML
    // *value* instead and rejects a document, which silently emptied this
    // allow-list.
    let Ok(value) = toml::from_str::<toml::Value>(&text) else {
        return out;
    };
    let mut declared_normalised: HashSet<String> = HashSet::new();
    for table in ["dependencies", "dev-dependencies"] {
        if let Some(toml::Value::Table(deps)) = value.get(table) {
            for name in deps.keys() {
                // The distribution name's first segment — a reasonable guess
                // for the import root, correct for most packages.
                out.insert(top_level(name).to_owned());
                declared_normalised.insert(pep503_normalise(name));
            }
        }
    }
    // Expand via installed `.dist-info` metadata so packages whose import
    // root differs from the PyPI name (`beautifulsoup4` → `bs4`,
    // `agent-framework-core` → `agent_framework`) are allow-listed too —
    // keeping `tyc build` and the LSP consistent with `tyc check`, which
    // already performs this expansion. A no-op without a `.venv`.
    for import_root in top_level_imports_from_venv(project_root, &declared_normalised) {
        out.insert(top_level(&import_root).to_owned());
    }
    out
}

/// Surface the un-introspectable declared dependencies returned by
/// [`enrich_project_shapes_with_venv`], honouring the
/// `[strictness] unintrospectable-dependency` severity (`"warn"` default /
/// `"error"` / `"off"`). Returns `true` when the severity is `"error"` and
/// there were offenders, so the caller can fail the build/check. Keeping the
/// missed checks visible is the whole point — a silently-skipped third-party
/// check looks identical to a clean pass.
pub fn report_unintrospectable_dependencies(packages: &[String], severity: &str) -> bool {
    if packages.is_empty() || severity == "off" {
        return false;
    }
    // `TYC_NO_INTROSPECT=1` switches introspection off deliberately — for a
    // sandbox, a hermetic CI job, or to keep a dependency's import-time code
    // from running. Failing the build because the thing the user turned off
    // did not happen makes the escape hatch unusable, so the severity
    // downgrades to a warning that says why.
    let suppressed = std::env::var_os("TYC_NO_INTROSPECT").is_some();
    let is_error = severity == "error" && !suppressed;
    let label = if is_error { "error" } else { "warning" };
    if suppressed && severity == "error" {
        eprintln!(
            "note: `TYC_NO_INTROSPECT` is set, so venv introspection did not run; \
`[strictness] unintrospectable-dependency = \"error\"` is reported as a warning \
rather than failing the build."
        );
    }
    eprintln!(
        "{label}: declared {} could not be introspected: {}\n  \
         third-party argument/type checks for {} were skipped. Install the project's \
         dependencies (e.g. `uv sync`) so `tyc` can read their signatures, add a `.dty` \
         stub, or set `[strictness] unintrospectable-dependency = \"off\"` to silence.",
        if packages.len() == 1 {
            "dependency"
        } else {
            "dependencies"
        },
        packages.join(", "),
        if packages.len() == 1 { "it" } else { "them" },
    );
    is_error
}

// ── dist→import name resolution (shared by `tyc check` / `tyc build` /
// LSP, so all three allow-list the same set of third-party imports) ──

/// Normalise a PyPI distribution name per PEP 503: replace runs of
/// `[-_.]+` with a single `-` and lowercase. `tyc` uses this to match
/// `[dependencies]` keys against `.dist-info` directory names regardless
/// of casing or separator drift (`Agent-Framework-Core`,
/// `agent_framework_core`, `agent.framework.core` all normalise the
/// same).
pub fn pep503_normalise(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_sep = false;
    for ch in name.chars() {
        if ch == '-' || ch == '_' || ch == '.' {
            if !prev_sep && !out.is_empty() {
                out.push('-');
                prev_sep = true;
            }
        } else {
            out.push(ch.to_ascii_lowercase());
            prev_sep = false;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}

/// Scan the project's local virtualenv for installed Python
/// distributions and return the top-level import names each one
/// provides — restricted to distributions actually declared in
/// `[dependencies]` / `[dev-dependencies]`. Used by `tyc check` to vet
/// imports whose package name differs from the PyPI distribution name
/// (e.g. `agent-framework-core` -> `agent_framework`,
/// `beautifulsoup4` -> `bs4`).
///
/// Looks for a venv at `<project_root>/.venv` — the default `uv` /
/// `tyc sync` location — and reads `<dist>-<ver>.dist-info/top_level.txt`
/// from each declared distribution's metadata. If `top_level.txt` is
/// absent (newer wheels often omit it) we fall back to scanning the
/// `RECORD` manifest for top-level `<pkg>/__init__.py` paths.
///
/// `declared` is a set of PEP 503-normalised distribution names; only
/// `.dist-info` directories whose dist-name normalises into that set
/// contribute import roots. This keeps `tyc check` reproducible across
/// machines: a developer with extra packages in their local venv
/// won't accidentally pass imports that would fail on a fresh clone.
///
/// Returns an empty list when no venv exists.
pub fn top_level_imports_from_venv(
    project_root: &std::path::Path,
    declared: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if declared.is_empty() {
        return out;
    }
    let venv_root = project_root.join(".venv");
    if !venv_root.is_dir() {
        return out;
    }
    // `site-packages` lives under `lib/pythonX.Y` on POSIX,
    // `Lib/site-packages` on Windows. Probe both.
    let mut site_dirs: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(venv_root.join("lib")) {
        for entry in entries.flatten() {
            let p = entry.path().join("site-packages");
            if p.is_dir() {
                site_dirs.push(p);
            }
        }
    }
    let win_site = venv_root.join("Lib").join("site-packages");
    if win_site.is_dir() {
        site_dirs.push(win_site);
    }
    for site in site_dirs {
        let Ok(entries) = std::fs::read_dir(&site) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let dir_name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let Some(stem) = dir_name.strip_suffix(".dist-info") else {
                continue;
            };
            // `<dist>-<version>.dist-info` — the dist-name is the part
            // before the LAST `-<version>` segment. Strip the trailing
            // `-…` component (versions are PEP 440 and don't contain `/`
            // or whitespace, so trimming the rightmost `-` block is safe).
            let dist_name = stem.rsplit_once('-').map(|(d, _)| d).unwrap_or(stem);
            let normalised = pep503_normalise(dist_name);
            if !declared.contains(&normalised) {
                continue;
            }
            // Prefer top_level.txt — one package name per line.
            let tlt = path.join("top_level.txt");
            if let Ok(content) = std::fs::read_to_string(&tlt) {
                for line in content.lines() {
                    let line = line.trim();
                    // Filter out empty lines and namespace-package markers.
                    // top_level.txt may also list dotted paths for
                    // sub-packages; we only need the root component.
                    if line.is_empty() {
                        continue;
                    }
                    let root = line.split('/').next().unwrap_or(line);
                    let root = root.split('.').next().unwrap_or(root);
                    if !root.is_empty() && !root.starts_with('_') {
                        out.push(root.to_owned());
                    }
                }
                continue;
            }
            // Fallback: derive top-level packages from RECORD.
            // Each entry is `<path>,<hash>,<size>`; the path may be
            // double-quoted (PEP 376) if it contains commas, may be a
            // relative `../bin/script` for installed-outside-site
            // files, or absolute. We accept only paths that point at
            // a Python module (`<name>.py`) or at a Python source file
            // inside a top-level directory (`<name>/.../*.py(i)`) — the
            // narrower filter avoids picking up shipped `bin/`,
            // `docs/`, or `tests/` directories.
            if let Ok(record) = std::fs::read_to_string(path.join("RECORD")) {
                for line in record.lines() {
                    let raw = line.split(',').next().unwrap_or("").trim();
                    let path_field = raw.trim_matches('"');
                    if path_field.is_empty()
                        || path_field.starts_with("../")
                        || path_field.starts_with("..\\")
                        || path_field.starts_with('/')
                        || path_field.starts_with('\\')
                    {
                        continue;
                    }
                    let head = path_field.split('/').next().unwrap_or(path_field);
                    if head.is_empty()
                        || head == "."
                        || head == ".."
                        || head.starts_with('_')
                        || head.ends_with(".dist-info")
                        || head.ends_with(".data")
                    {
                        continue;
                    }
                    let import_name = if let Some(stem) = head.strip_suffix(".py") {
                        // Top-level single-file module (`foo.py`).
                        stem
                    } else if head.ends_with(".pyi") {
                        // Pure-stub package — uncommon but valid.
                        head.strip_suffix(".pyi").unwrap_or(head)
                    } else if path_field.contains('/')
                        && (path_field.ends_with(".py") || path_field.ends_with(".pyi"))
                    {
                        // Python source nested under a top-level
                        // directory — treat the directory as a package.
                        head
                    } else {
                        continue;
                    };
                    if !import_name.is_empty() && !import_name.starts_with('_') {
                        out.push(import_name.to_owned());
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_top_level_reads_declared_dependencies() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("typhon.toml"),
            "[project]\nname = \"x\"\n\n[dependencies]\nrequests = \">=2\"\n\n[dev-dependencies]\npytest = \"8\"\n",
        )
        .unwrap();
        let allowed = allowed_top_level_from_project(tmp.path());
        assert!(allowed.contains("requests"), "got {allowed:?}");
        assert!(
            allowed.contains("pytest"),
            "dev-deps count too: {allowed:?}"
        );
        // A project with no typhon.toml yields an empty allow-list (enrichment
        // is then a no-op), never a panic.
        let empty = tempfile::tempdir().unwrap();
        assert!(allowed_top_level_from_project(empty.path()).is_empty());
    }

    fn p(name: &str, kind: &str, has_default: bool) -> IntrospectedParam {
        IntrospectedParam {
            name: name.into(),
            kind: kind.into(),
            has_default,
            default_is_none: false,
            annotation: None,
        }
    }

    /// Like [`p`] but carries an annotation string, for the
    /// annotation-capture tests.
    fn p_ann(name: &str, kind: &str, has_default: bool, annotation: &str) -> IntrospectedParam {
        IntrospectedParam {
            name: name.into(),
            kind: kind.into(),
            has_default,
            default_is_none: false,
            annotation: Some(annotation.into()),
        }
    }

    /// Like [`p_ann`] but marks the parameter's default as literally `None`
    /// (the implicit-Optional idiom `x: int = None`).
    fn p_ann_none(name: &str, kind: &str, annotation: &str) -> IntrospectedParam {
        IntrospectedParam {
            name: name.into(),
            kind: kind.into(),
            has_default: true,
            default_is_none: true,
            annotation: Some(annotation.into()),
        }
    }

    #[test]
    fn class_shape_models_required_kwonly_params() {
        // `class Agent: __init__(*, name, client, tools=None)` — the
        // shape must mark `name` and `client` as required (no default)
        // and `tools` as optional. This is the user's reported case
        // (`Agent(...)` missing the `client` kwarg passed `tyc check`).
        let params = vec![
            p("name", "keyword_only", false),
            p("client", "keyword_only", false),
            p("tools", "keyword_only", true),
        ];
        let shape = class_shape_from_params(&params).expect("shape");
        assert_eq!(shape.field_order, vec!["name", "client", "tools"]);
        assert!(!shape.field_defaults.contains("name"));
        assert!(!shape.field_defaults.contains("client"));
        assert!(shape.field_defaults.contains("tools"));
    }

    #[test]
    fn class_shape_skipped_when_var_keyword_present() {
        // **kwargs defeats the field-based constructor check —
        // returning None ensures we don't fire false positives on
        // permissive Python APIs.
        let params = vec![
            p("name", "keyword_only", false),
            p("extras", "var_keyword", false),
        ];
        assert!(class_shape_from_params(&params).is_none());
    }

    #[test]
    fn class_shape_skipped_when_var_positional_present() {
        let params = vec![
            p("first", "positional_or_keyword", false),
            p("rest", "var_positional", false),
        ];
        assert!(class_shape_from_params(&params).is_none());
    }

    #[test]
    fn arity_info_separates_positional_and_keyword_only() {
        // `def f(a, b=2, *, c, d=3)` — `a` required positional, `b`
        // defaulted, `c` required kw-only, `d` defaulted kw-only.
        let params = vec![
            p("a", "positional_or_keyword", false),
            p("b", "positional_or_keyword", true),
            p("c", "keyword_only", false),
            p("d", "keyword_only", true),
        ];
        let info = arity_info_from_params(&params, None).expect("info");
        assert_eq!(info.param_names, vec!["a", "b"]);
        assert_eq!(info.required_positional, vec![true, false]);
        assert_eq!(info.min_positional, 1);
        assert_eq!(info.max_positional, Some(2));
        assert_eq!(info.kwonly_names, vec!["c", "d"]);
        assert_eq!(info.kwonly_required, vec!["c"]);
        assert!(!info.has_kwarg);
    }

    #[test]
    fn arity_info_handles_var_positional_and_var_keyword() {
        let params = vec![
            p("a", "positional_or_keyword", false),
            p("args", "var_positional", false),
            p("kwargs", "var_keyword", false),
        ];
        let info = arity_info_from_params(&params, None).expect("info");
        assert_eq!(info.param_names, vec!["a"]);
        assert_eq!(info.max_positional, None, "vararg uncaps the maximum");
        assert!(info.has_kwarg, "**kwargs accepted");
    }

    #[test]
    fn shapes_from_introspected_skips_value_members() {
        // Plain values (constants, modules) carry no signature — the
        // conversion must ignore them rather than emit empty shapes.
        let intro = IntrospectedModule {
            members: vec![
                IntrospectedMember {
                    name: "VERSION".into(),
                    kind: "value".into(),
                    params: None,
                    returns: None,
                    methods: None,
                },
                IntrospectedMember {
                    name: "Agent".into(),
                    kind: "class".into(),
                    params: Some(vec![p("name", "keyword_only", false)]),
                    returns: None,
                    methods: None,
                },
            ],
        };
        let shapes = shapes_from_introspected(&intro);
        assert!(shapes.class_shapes.contains_key("Agent"));
        assert!(!shapes.class_shapes.contains_key("VERSION"));
    }

    #[test]
    fn introspected_methods_populate_class_shape() {
        // A class member's methods become arity-checkable MethodSigs on the
        // InterfaceShape, so `obj.fit(...)` (not just `Cls(...)`) is covered.
        let intro = IntrospectedModule {
            members: vec![IntrospectedMember {
                name: "PCA".into(),
                kind: "class".into(),
                params: Some(vec![p("n_components", "positional_or_keyword", true)]),
                returns: None,
                methods: Some(vec![
                    IntrospectedMethod {
                        name: "fit".into(),
                        kind: "method".into(),
                        // receiver already stripped on the Python side
                        params: vec![
                            p("X", "positional_or_keyword", false),
                            p("y", "positional_or_keyword", true),
                        ],
                        returns: None,
                    },
                    IntrospectedMethod {
                        name: "set_params".into(),
                        kind: "method".into(),
                        params: vec![p("params", "var_keyword", false)],
                        returns: None,
                    },
                ]),
            }],
        };
        let shapes = shapes_from_introspected(&intro);
        let pca = shapes.class_shapes.get("PCA").expect("PCA shape");
        let fit = pca.methods.get("fit").expect("fit method captured");
        assert_eq!(fit.arity_info.min_positional, 1, "X required, y optional");
        assert_eq!(fit.arity_info.max_positional, Some(2));
        assert!(!fit.is_static && !fit.is_classmethod);
        // `set_params(**params)` stays permissive (no required positionals).
        let sp = pca.methods.get("set_params").expect("set_params captured");
        assert_eq!(sp.arity_info.min_positional, 0);
        assert!(sp.arity_info.has_kwarg);
    }

    #[test]
    fn annotation_to_type_maps_scalars_and_degrades_safely() {
        assert_eq!(annotation_to_type("int"), Type::Int);
        assert_eq!(annotation_to_type("<class 'str'>"), Type::Str);
        assert_eq!(annotation_to_type("builtins.bool"), Type::Bool);
        assert_eq!(annotation_to_type("float"), Type::Float);
        assert_eq!(annotation_to_type("bytes"), Type::Bytes);
        assert_eq!(annotation_to_type("None"), Type::None);
        assert_eq!(annotation_to_type("NoneType"), Type::None);
        // Nullable scalar forms map to `T | None`.
        assert_eq!(
            annotation_to_type("Optional[str]"),
            Type::Union(vec![Type::Str, Type::None])
        );
        assert_eq!(
            annotation_to_type("typing.Optional[int]"),
            Type::Union(vec![Type::Int, Type::None])
        );
        assert_eq!(
            annotation_to_type("int | None"),
            Type::Union(vec![Type::Int, Type::None])
        );
        assert_eq!(
            annotation_to_type("None | bytes"),
            Type::Union(vec![Type::Bytes, Type::None])
        );
        // Parametric containers map element-wise.
        assert_eq!(
            annotation_to_type("list[int]"),
            Type::Generic("list".into(), vec![Type::Int])
        );
        assert_eq!(
            annotation_to_type("List[str]"),
            Type::Generic("list".into(), vec![Type::Str])
        );
        assert_eq!(
            annotation_to_type("dict[str, int]"),
            Type::Generic("dict".into(), vec![Type::Str, Type::Int])
        );
        assert_eq!(
            annotation_to_type("set[bytes]"),
            Type::Generic("set".into(), vec![Type::Bytes])
        );
        assert_eq!(
            annotation_to_type("list[list[int]]"),
            Type::Generic(
                "list".into(),
                vec![Type::Generic("list".into(), vec![Type::Int])]
            )
        );
        assert_eq!(
            annotation_to_type("Optional[list[int]]"),
            Type::Union(vec![
                Type::Generic("list".into(), vec![Type::Int]),
                Type::None
            ])
        );
        // A container whose element doesn't map degrades the whole thing to
        // Unknown — never `list[Unknown]`.
        assert_eq!(annotation_to_type("list[requests.Session]"), Type::Unknown);
        assert_eq!(annotation_to_type("dict[str, Session]"), Type::Unknown);
        // Fixed-arity tuples map element-wise; homogeneous `tuple[X, ...]`,
        // Callable, and foreign generics stay permissive.
        assert_eq!(
            annotation_to_type("tuple[int, str]"),
            Type::Generic("tuple".into(), vec![Type::Int, Type::Str])
        );
        assert_eq!(annotation_to_type("tuple[int, ...]"), Type::Unknown);
        assert_eq!(annotation_to_type("Callable[[int], str]"), Type::Unknown);
        // 3+-member unions stay permissive (deferred); foreign names too.
        assert_eq!(annotation_to_type("int | str | None"), Type::Unknown);
        assert_eq!(annotation_to_type("int | str | bytes"), Type::Unknown);
        assert_eq!(annotation_to_type("requests.Session"), Type::Unknown);
    }

    #[test]
    fn small_non_nullable_unions_map_to_real_union() {
        // A2: a 2-member non-nullable union becomes a real `Type::Union`, in
        // both the `X | Y` (PEP 604) and `Union[X, Y]` (PEP 484) spellings.
        // These are the jinja2 `Union[str, bytes]` / Flask `Union[str,
        // PathLike]`-shaped params that previously degraded to `Unknown`.
        assert_eq!(
            annotation_to_type("str | bytes"),
            Type::Union(vec![Type::Str, Type::Bytes])
        );
        assert_eq!(
            annotation_to_type("Union[str, bytes]"),
            Type::Union(vec![Type::Str, Type::Bytes])
        );
        assert_eq!(
            annotation_to_type("typing.Union[int, str]"),
            Type::Union(vec![Type::Int, Type::Str])
        );
        // Member order is preserved.
        assert_eq!(
            annotation_to_type("int | str"),
            Type::Union(vec![Type::Int, Type::Str])
        );
        // Concrete container members are fine.
        assert_eq!(
            annotation_to_type("str | list[int]"),
            Type::Union(vec![
                Type::Str,
                Type::Generic("list".into(), vec![Type::Int])
            ])
        );
    }

    #[test]
    fn small_union_with_unresolvable_member_degrades() {
        // SOUNDNESS: if ANY member is unresolvable the whole union degrades to
        // `Unknown` (permissive) — never reject a value on a union we can't
        // fully model. This is the hard guard against false positives.
        assert_eq!(annotation_to_type("str | requests.Session"), Type::Unknown);
        assert_eq!(annotation_to_type("Union[int, Session]"), Type::Unknown);
        assert_eq!(
            annotation_to_type("Foo | Bar"),
            Type::Unknown,
            "two foreign members ⇒ Unknown"
        );
        // A container with an unresolvable element is itself Unknown, so the
        // union degrades too.
        assert_eq!(annotation_to_type("str | list[Session]"), Type::Unknown);
    }

    #[test]
    fn union_nullable_and_redundant_forms_reduce() {
        // `Union[X, None]` is just the nullable form `X | None`.
        assert_eq!(
            annotation_to_type("Union[str, None]"),
            Type::Union(vec![Type::Str, Type::None])
        );
        // Redundant members dedup down to the simpler shape.
        assert_eq!(annotation_to_type("int | int"), Type::Int);
        assert_eq!(
            annotation_to_type("Union[str, str, bytes]"),
            Type::Union(vec![Type::Str, Type::Bytes])
        );
        // A non-nullable member alongside None with another member is the
        // deferred `X | Y | None` shape — permissive.
        assert_eq!(annotation_to_type("int | str | None"), Type::Unknown);
    }

    #[test]
    fn annotation_unwraps_annotated_to_first_type_arg() {
        // `Annotated[T, meta…]` resolves to `T`, discarding metadata — this is
        // the FastAPI / Typer / Pydantic param form. A1.
        assert_eq!(annotation_to_type("Annotated[int, \"meta\"]"), Type::Int);
        assert_eq!(
            annotation_to_type("typing.Annotated[str, 'meta']"),
            Type::Str
        );
        assert_eq!(
            annotation_to_type("typing_extensions.Annotated[bool, 'x']"),
            Type::Bool
        );
        // Metadata carrying its own commas / brackets / parens (the typical
        // `repr()` of a Pydantic / FastAPI marker) must not derail the split:
        // only the first top-level segment is the type.
        assert_eq!(
            annotation_to_type(
                "typing.Annotated[str, FieldInfo(default=PydanticUndefined, extra={})]"
            ),
            Type::Str
        );
        assert_eq!(
            annotation_to_type("Annotated[int, Gt(0), Lt(10)]"),
            Type::Int
        );
        // Nested `Annotated[Optional[int], …]` resolves through to the inner
        // nullable type.
        assert_eq!(
            annotation_to_type("Annotated[Optional[int], 'meta']"),
            Type::Union(vec![Type::Int, Type::None])
        );
        assert_eq!(
            annotation_to_type("Annotated[list[int], 'meta']"),
            Type::Generic("list".into(), vec![Type::Int])
        );
        // If the wrapped type doesn't resolve we degrade to Unknown exactly as
        // before — never stricter than today.
        assert_eq!(
            annotation_to_type("Annotated[requests.Session, 'meta']"),
            Type::Unknown
        );
    }

    #[test]
    fn annotation_capture_populates_param_and_return_types() {
        // `def get(url: str, timeout: float = ...) -> SomethingForeign`
        let params = vec![
            p_ann("url", "positional_or_keyword", false, "str"),
            p_ann("timeout", "positional_or_keyword", true, "float"),
        ];
        let info = arity_info_from_params(&params, Some("Response")).unwrap();
        assert_eq!(info.param_types, vec![Type::Str, Type::Float]);
        // Unrecognised return annotation degrades to Unknown.
        assert_eq!(info.return_type, Type::Unknown);
    }

    #[test]
    fn implicit_optional_default_none_widens_param_to_nullable() {
        // `def f(x: int = None, y: str = None)` — the bare-scalar annotations
        // "lie" (None is valid). Both must widen to `T | None` so a `None`
        // argument doesn't false-positive, while a genuinely wrong-typed arg
        // still fails against the nullable form.
        let params = vec![
            p_ann_none("x", "positional_or_keyword", "int"),
            p_ann_none("y", "keyword_only", "str"),
        ];
        let info = arity_info_from_params(&params, None).unwrap();
        assert_eq!(
            info.param_types,
            vec![Type::Union(vec![Type::Int, Type::None])]
        );
        assert_eq!(
            info.kwonly_types,
            vec![Type::Union(vec![Type::Str, Type::None])]
        );
        // The same widening flows into a constructor field type.
        let shape = class_shape_from_params(&params).unwrap();
        assert_eq!(
            shape.fields.get("x"),
            Some(&Type::Union(vec![Type::Int, Type::None]))
        );
        // An already-Optional annotation with a None default isn't double-wrapped.
        let opt = vec![p_ann_none("z", "positional_or_keyword", "Optional[int]")];
        let info2 = arity_info_from_params(&opt, None).unwrap();
        assert_eq!(
            info2.param_types,
            vec![Type::Union(vec![Type::Int, Type::None])]
        );
        // A param with a non-None default keeps its bare type (the widening is
        // keyed on the default being literally `None`, not on having a default).
        let non_none = vec![p_ann("w", "positional_or_keyword", true, "int")];
        let info3 = arity_info_from_params(&non_none, None).unwrap();
        assert_eq!(info3.param_types, vec![Type::Int]);
    }

    #[test]
    fn annotation_capture_populates_constructor_field_types() {
        // `class C: __init__(self, host: str, port: int)`
        let params = vec![
            p_ann("host", "positional_or_keyword", false, "str"),
            p_ann("port", "positional_or_keyword", false, "int"),
        ];
        let shape = class_shape_from_params(&params).unwrap();
        assert_eq!(shape.fields.get("host"), Some(&Type::Str));
        assert_eq!(shape.fields.get("port"), Some(&Type::Int));
    }

    #[test]
    fn collect_imported_modules_finds_every_form() {
        // Cover `import X`, `import X.Y`, `import X as Y`, comma-
        // separated imports, `from X import Y`, and `lazy import
        // np = numpy`. The result is sorted and deduplicated.
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("a.ty"),
            "\
import os
import collections.abc
import json as j, csv
from agent_framework import Agent
from agent_framework.openai import OpenAIChatClient
lazy import np = numpy
\nNOT_AN_IMPORT = 1
",
        )
        .unwrap();
        let modules = collect_imported_modules(&[src]);
        // Both subpackages and the comma-second-element are captured.
        assert!(modules.contains(&"os".into()));
        assert!(modules.contains(&"collections.abc".into()));
        assert!(modules.contains(&"json".into()));
        assert!(modules.contains(&"csv".into()));
        assert!(modules.contains(&"agent_framework".into()));
        assert!(modules.contains(&"agent_framework.openai".into()));
        assert!(modules.contains(&"numpy".into()));
        // Non-import lines are not picked up.
        assert!(!modules.iter().any(|m| m.starts_with("NOT_AN_IMPORT")));
    }

    #[test]
    fn is_valid_dotted_name_rejects_garbage() {
        assert!(is_valid_dotted_name("foo"));
        assert!(is_valid_dotted_name("foo.bar"));
        assert!(is_valid_dotted_name("_foo._bar"));
        assert!(!is_valid_dotted_name("1foo"));
        assert!(!is_valid_dotted_name("foo..bar"));
        assert!(!is_valid_dotted_name(""));
        assert!(!is_valid_dotted_name("foo-bar"));
    }

    #[test]
    fn venv_signatures_skips_modules_outside_allow_list() {
        // The allow-list is the gate that keeps stdlib introspection
        // from running on `import os`. We can verify the gating
        // logic without actually spawning Python.
        let tmp = tempfile::tempdir().unwrap();
        let mut allowed: HashSet<String> = HashSet::new();
        allowed.insert("agent_framework".into());
        let mut cache = VenvSignatures::for_project_root(tmp.path(), allowed);
        // `os` not in allowed → never invokes Python.
        assert!(cache.module_shapes("os").is_none());
        assert!(!cache.cache.contains_key("os"));
        // Sub-modules of an allowed top-level *are* gated through.
        // We don't assert on the result (depends on the host's
        // Python and venv state), only that the gate matched.
        let allowed_again: HashSet<String> = ["agent_framework".to_owned()].into_iter().collect();
        let cache2 = VenvSignatures::for_project_root(tmp.path(), allowed_again);
        let top = "agent_framework.openai".split('.').next().unwrap();
        assert!(cache2.allowed_top_level.contains(top));
    }

    #[test]
    #[cfg(unix)]
    fn preload_batches_all_modules_into_one_subprocess() {
        // The performance fix relies on Python being spawned once
        // per `tyc check` invocation. Rather than time the call and
        // hope CI is fast enough (the original timing-based check
        // was flagged as flaky in PR #98 review), point
        // `VenvSignatures` at a stub script that appends one line
        // per invocation to a counter file and emits the JSON shape
        // the Rust side expects. Asserting `wc -l == 1` is
        // deterministic regardless of host speed.
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let counter = tmp.path().join("invocations.log");
        let stub = tmp.path().join("fake-python");
        // The Rust side feeds the embedded INTROSPECT_SCRIPT on
        // stdin (because `python -` was the original interpreter
        // invocation), so discard stdin and just emit the expected
        // batched-output envelope. The script also records each
        // invocation so the test can count spawns precisely.
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\n\
                 echo invoked >> {log}\n\
                 cat > /dev/null\n\
                 # Echo back an empty members object for every module argument.\n\
                 printf '{{'\n\
                 first=1\n\
                 for arg in \"$@\"; do\n\
                 case \"$arg\" in -) continue;; esac\n\
                 if [ $first -eq 1 ]; then first=0; else printf ','; fi\n\
                 printf '\"%s\": {{\"members\": []}}' \"$arg\"\n\
                 done\n\
                 printf '}}'\n",
                log = counter.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let allowed: HashSet<String> = ["pkg_a", "pkg_b", "pkg_c", "pkg_d", "pkg_e"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let mut cache = VenvSignatures {
            python_bin: Some(stub),
            project_root: tmp.path().to_path_buf(),
            scratch: ScratchDir::new(),
            allowed_top_level: allowed,
            cache: HashMap::new(),
            venv_stamp: None,
        };
        let modules: Vec<String> = ["pkg_a", "pkg_b", "pkg_c", "pkg_d", "pkg_e"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        cache.preload(&modules);
        // Every requested name lands in the cache, proving the
        // batched call wired the JSON back out correctly.
        for m in &modules {
            assert!(
                cache.cache.contains_key(m),
                "preload should populate cache for `{m}`"
            );
        }
        // Exactly one subprocess spawn — a per-module regression
        // would write five lines into the counter file.
        let log = std::fs::read_to_string(&counter).unwrap_or_default();
        let invocations = log.lines().count();
        assert_eq!(
            invocations, 1,
            "expected one batched subprocess; got {invocations} (log: {log:?})"
        );
    }

    #[test]
    #[cfg(unix)]
    fn preload_falls_back_to_per_module_on_batch_failure() {
        // If the batched subprocess fails (timeout, malformed
        // stdout, …), each module should be retried in isolation
        // so one pathological import doesn't poison every
        // unrelated dependency in the run. (Codex P1 review of
        // PR #98.) The stub here errors on multi-module argv but
        // succeeds for any single module, mirroring the
        // "one heavy import broke the batch" scenario.
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let counter = tmp.path().join("invocations.log");
        let stub = tmp.path().join("fake-python");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\n\
                 echo invoked >> {log}\n\
                 cat > /dev/null\n\
                 # Count positional args, skipping the leading '-' that\n\
                 # marks 'read script from stdin'.\n\
                 mods=0\n\
                 for arg in \"$@\"; do\n\
                 case \"$arg\" in -) continue;; esac\n\
                 mods=$((mods + 1))\n\
                 done\n\
                 if [ $mods -gt 1 ]; then\n\
                 # Simulate a broken batch run.\n\
                 exit 1\n\
                 fi\n\
                 printf '{{'\n\
                 first=1\n\
                 for arg in \"$@\"; do\n\
                 case \"$arg\" in -) continue;; esac\n\
                 if [ $first -eq 1 ]; then first=0; else printf ','; fi\n\
                 printf '\"%s\": {{\"members\": []}}' \"$arg\"\n\
                 done\n\
                 printf '}}'\n",
                log = counter.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let allowed: HashSet<String> = ["pkg_a", "pkg_b", "pkg_c"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let mut cache = VenvSignatures {
            python_bin: Some(stub),
            project_root: tmp.path().to_path_buf(),
            scratch: ScratchDir::new(),
            allowed_top_level: allowed,
            cache: HashMap::new(),
            venv_stamp: None,
        };
        let modules: Vec<String> = ["pkg_a", "pkg_b", "pkg_c"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        cache.preload(&modules);
        // All three modules end up in the cache because the
        // per-module fallback succeeded for each.
        for m in &modules {
            assert!(
                cache.cache.contains_key(m),
                "fallback should populate cache for `{m}`"
            );
        }
        // One batched attempt + three single-module retries = 4
        // invocations. If the fallback regressed (e.g. someone
        // reverted to caching every module as a miss on batch
        // failure), only one invocation would appear.
        let log = std::fs::read_to_string(&counter).unwrap_or_default();
        let invocations = log.lines().count();
        assert_eq!(
            invocations, 4,
            "expected 1 batched + 3 per-module fallback spawns; got {invocations} (log: {log:?})"
        );
    }

    #[test]
    fn introspection_survives_a_member_that_raises_on_signature() {
        // Regression: a module member that raises a NON-(TypeError|ValueError)
        // from `inspect.signature` (the canonical case is a werkzeug
        // `LocalProxy` re-exported at module scope — `flask.current_app` / `g`
        // / `request` / `session`) used to crash the *entire* module's
        // introspection, so Flask's constructors/functions all went unchecked.
        // The embedded script must now skip the bad member and still recover
        // the module's real classes/functions. Driven end-to-end against a
        // real Python because the in-crate harness doesn't spawn the venv
        // introspection itself.
        let Some(python) = which_python3() else {
            return; // no Python on PATH — nothing to verify here.
        };
        let tmp = tempfile::tempdir().unwrap();
        // A callable module-level value whose `__signature__` raises
        // RuntimeError on access — exactly how a werkzeug LocalProxy behaves
        // outside an application context — plus a genuine class we must still
        // recover.
        std::fs::write(
            tmp.path().join("flaskish.py"),
            "\
class _Proxy:
    def __call__(self):
        return None
    @property
    def __signature__(self):
        raise RuntimeError('Working outside of application context.')

current_app = _Proxy()

class App:
    def __init__(self, import_name):
        self.import_name = import_name
",
        )
        .unwrap();
        let result = introspect_batch_via_python(
            &python,
            tmp.path(),
            std::slice::from_ref(&"flaskish".to_owned()),
        )
        .expect("introspection batch should succeed despite the raising member");
        let module = result.get("flaskish").expect("flaskish module present");
        // The proxy member must not have crashed the run: the real class is
        // recovered with its required constructor parameter.
        let app = module
            .members
            .iter()
            .find(|m| m.name == "App")
            .expect("App class recovered despite the raising proxy member");
        assert_eq!(app.kind, "class");
        let params = app.params.as_ref().expect("App __init__ params captured");
        assert!(
            params.iter().any(|p| p.name == "import_name"),
            "App's required `import_name` param must be captured: {params:?}"
        );
        // And the shape conversion yields a checkable constructor.
        let shapes = shapes_from_introspected(module);
        let shape = shapes
            .class_shapes
            .get("App")
            .expect("App shape built from introspection");
        assert!(shape.field_order.contains(&"import_name".to_owned()));
        assert!(!shape.field_defaults.contains("import_name"));
    }

    #[test]
    fn scratch_dir_is_fresh_private_and_cleaned_up() {
        let a = ScratchDir::new().expect("temp dir must be writable in tests");
        let b = ScratchDir::new().expect("temp dir must be writable in tests");
        // Fresh and empty — nothing importable inside.
        assert!(a.path().is_dir());
        assert!(std::fs::read_dir(a.path()).unwrap().next().is_none());
        // Per-instance, never a fixed shared path.
        assert_ne!(a.path(), b.path());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(a.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "scratch dir must be private to the user");
            // The *atomicity* of that mode — 0700 passed to `mkdir(2)` rather
            // than chmod'd on afterwards — cannot be observed from inside this
            // process: the window it closes sits between two syscalls. What
            // this assertion does guard is the fail-closed check that backs
            // it: a regression to a bare `create_dir` would, under a
            // permissive umask, produce a non-private directory that
            // `ScratchDir::new` now refuses outright, so `expect` above fails
            // rather than handing back a world-writable cwd.
        }
        // Dropping removes the directory (and anything a subprocess left in it).
        std::fs::write(a.path().join("leftover.txt"), "x").unwrap();
        let a_path = a.path().to_path_buf();
        drop(a);
        assert!(!a_path.exists(), "scratch dir must be removed on drop");
    }

    #[test]
    fn introspection_does_not_run_in_the_project_root() {
        // The import-shadowing regression `SECURITY.md` rules out: the
        // embedded helper's first statement imports stdlib modules
        // (`import sys, json, …`), and for a stdin script `sys.path[0]` is
        // the subprocess cwd. When that cwd was the project root, an
        // attacker-controlled `<root>/json.py` executed with the user's
        // privileges on any `tyc check` of the repo. The subprocess must now
        // run in an empty scratch directory instead.
        let Some(_python) = which_python3() else {
            return; // no Python on PATH — nothing to verify here.
        };
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let marker = root.join("pwned.txt");
        std::fs::write(
            root.join("json.py"),
            format!(
                "open({:?}, 'w').write('shadowed stdlib json executed')\n",
                marker.to_string_lossy()
            ),
        )
        .unwrap();
        let allowed: HashSet<String> = ["definitely_not_installed_pkg_zz".to_owned()]
            .into_iter()
            .collect();
        let mut cache = VenvSignatures::for_project_root(root, allowed);
        cache.preload(&["definitely_not_installed_pkg_zz".to_owned()]);
        assert!(
            !marker.exists(),
            "project-root json.py was executed: the introspection subprocess \
             ran with the project root as cwd"
        );
    }

    #[test]
    fn shapes_from_introspected_skips_classes_with_no_signature() {
        // `inspect.signature` raised for this class (C extension).
        // We must not emit a shape — a missing arity check is safer
        // than a wrong one.
        let intro = IntrospectedModule {
            members: vec![IntrospectedMember {
                name: "WeirdCType".into(),
                kind: "class".into(),
                params: None,
                returns: None,
                methods: None,
            }],
        };
        let shapes = shapes_from_introspected(&intro);
        assert!(shapes.class_shapes.is_empty());
    }

    /// `TYC_NO_INTROSPECT` turns introspection off on purpose. Failing the
    /// build because the thing the user disabled did not happen makes the
    /// escape hatch unusable, so `"error"` downgrades to a warning.
    /// Rust runs tests in parallel and `set_var` / `remove_var` mutate
    /// process-global state, so an env-mutating test has to serialise
    /// against every other one in this binary.
    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn no_introspect_downgrades_the_error_severity() {
        let _guard = lock_env();
        let pkgs = vec!["somepkg".to_owned()];
        let previous = std::env::var_os("TYC_NO_INTROSPECT");
        // SAFETY: single-threaded test body; restored before returning.
        unsafe { std::env::remove_var("TYC_NO_INTROSPECT") };
        assert!(
            report_unintrospectable_dependencies(&pkgs, "error"),
            "without the escape hatch, `error` must fail the build"
        );
        // SAFETY: as above.
        unsafe { std::env::set_var("TYC_NO_INTROSPECT", "1") };
        assert!(
            !report_unintrospectable_dependencies(&pkgs, "error"),
            "with introspection disabled, `error` must downgrade to a warning"
        );
        assert!(!report_unintrospectable_dependencies(&pkgs, "warning"));
        assert!(!report_unintrospectable_dependencies(&[], "error"));
        assert!(!report_unintrospectable_dependencies(&pkgs, "off"));
        match previous {
            // SAFETY: as above.
            Some(v) => unsafe { std::env::set_var("TYC_NO_INTROSPECT", v) },
            None => unsafe { std::env::remove_var("TYC_NO_INTROSPECT") },
        }
    }
}
