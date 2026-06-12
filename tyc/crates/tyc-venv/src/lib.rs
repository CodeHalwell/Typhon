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

use tyc_types::{ArityInfo, InterfaceShape, ModuleShapes, Type};

/// One parameter recovered from `inspect.signature`.
#[derive(Debug, Clone, Deserialize)]
struct IntrospectedParam {
    name: String,
    /// One of `"positional_only"`, `"positional_or_keyword"`,
    /// `"var_positional"` (i.e. `*args`), `"keyword_only"`,
    /// `"var_keyword"` (i.e. `**kwargs`).
    kind: String,
    has_default: bool,
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
    /// Working directory the introspection subprocess is spawned in.
    /// Pinning this to the project root (rather than inheriting the
    /// parent's CWD) keeps `tyc check` reproducible — the same
    /// command produces the same shape registry regardless of which
    /// subdirectory the user ran it from. Python's `sys.path[0]`
    /// also picks up packages siblings of this directory, which is
    /// the documented expectation.
    cwd: PathBuf,
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
            cwd: project_root.to_path_buf(),
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
        let current = stat_pyvenv_cfg(&self.cwd);
        if current != self.venv_stamp {
            self.cache.clear();
            self.python_bin = discover_python(&self.cwd);
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
        match introspect_batch_via_python(python, &self.cwd, &to_fetch) {
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
                    let shapes =
                        introspect_batch_via_python(python, &self.cwd, std::slice::from_ref(&name))
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
/// own `.venv/bin/python`, falling back to a `python3` on PATH.
fn discover_python(project_root: &Path) -> Option<PathBuf> {
    let venv_python = project_root.join(".venv").join("bin").join("python");
    if venv_python.is_file() {
        Some(venv_python)
    } else {
        which_python3()
    }
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
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("python3");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
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
    except (TypeError, ValueError):
        return None
    out = []
    for p in sig.parameters.values():
        out.append({
            "name": p.name,
            "kind": PARAM_KIND_MAP.get(p.kind, "positional_or_keyword"),
            "has_default": p.default is not inspect.Parameter.empty,
            "annotation": ann_to_str(p.annotation),
        })
    return out

def returns_of(obj):
    try:
        sig = inspect.signature(obj)
    except (TypeError, ValueError):
        return None
    return ann_to_str(sig.return_annotation)

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
        kind = kind_of(obj)
        members.append({
            "name": name,
            "kind": kind,
            "params": params_of(obj) if kind in ("class", "function") else None,
            "returns": returns_of(obj) if kind == "function" else None,
        })
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
    let mut stdout = child.stdout.take()?;
    let drainer = std::thread::spawn(move || -> Vec<u8> {
        let mut buf = Vec::with_capacity(64 * 1024);
        let _ = stdout.read_to_end(&mut buf);
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
                if let Some(shape) = class_shape_from_params(params) {
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
        fields.insert(
            p.name.clone(),
            p.annotation
                .as_deref()
                .map(annotation_to_type)
                .unwrap_or(Type::Unknown),
        );
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
/// - fully-concrete parametric containers `list[X]` / `set[X]` /
///   `frozenset[X]` / `dict[K, V]` and fixed-arity `tuple[T1, …]`
///   (recursively — a container whose element doesn't resolve degrades the
///   whole thing).
///
/// Everything else — multi-member unions, `tuple[T, ...]`, `Callable`,
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

    // `Optional[X]` (optionally `typing.`-qualified) → `X | None`.
    let unqualified = s.strip_prefix("typing.").unwrap_or(s);
    if let Some(inner) = unqualified
        .strip_prefix("Optional[")
        .and_then(|r| r.strip_suffix(']'))
    {
        return nullable_of(annotation_to_type(inner));
    }

    // `X | None` / `None | X` (PEP 604) — the 2-member nullable form. Richer
    // unions stay permissive (`Unknown`) so we never false-positive.
    if let Some((a, b)) = split_top_level_union(s) {
        let (a, b) = (a.trim(), b.trim());
        if b == "None" || b == "NoneType" {
            return nullable_of(annotation_to_type(a));
        }
        if a == "None" || a == "NoneType" {
            return nullable_of(annotation_to_type(b));
        }
        return Type::Unknown;
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

/// Wrap `inner` as nullable (`inner | None`). A non-mappable inner collapses
/// to `Unknown` (a nullable-`Unknown` is just `Unknown` — still permissive).
fn nullable_of(inner: Type) -> Type {
    match inner {
        Type::Unknown | Type::Any => Type::Unknown,
        Type::None => Type::None,
        other => Type::Union(vec![other, Type::None]),
    }
}

/// Split `s` on a single top-level `|` (ignoring pipes nested inside
/// `[...]` / `(...)`), returning the two halves. Used to recognise the
/// `X | None` nullable form. `None` when there is no top-level pipe.
fn split_top_level_union(s: &str) -> Option<(&str, &str)> {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'[' | b'(' => depth += 1,
            b']' | b')' => depth -= 1,
            b'|' if depth == 0 => return Some((&s[..i], &s[i + 1..])),
            _ => {}
        }
    }
    None
}

/// Build an [`ArityInfo`] for an introspected free function. `returns` is
/// the function's return annotation (or `None`).
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
    for p in params {
        let ty = p
            .annotation
            .as_deref()
            .map(annotation_to_type)
            .unwrap_or(Type::Unknown);
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
    let Ok(value) = text.parse::<toml::Value>() else {
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
    let is_error = severity == "error";
    let label = if is_error { "error" } else { "warning" };
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
                },
                IntrospectedMember {
                    name: "Agent".into(),
                    kind: "class".into(),
                    params: Some(vec![p("name", "keyword_only", false)]),
                    returns: None,
                },
            ],
        };
        let shapes = shapes_from_introspected(&intro);
        assert!(shapes.class_shapes.contains_key("Agent"));
        assert!(!shapes.class_shapes.contains_key("VERSION"));
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
        // Anything else we don't confidently model degrades to Unknown.
        assert_eq!(annotation_to_type("int | str | None"), Type::Unknown);
        assert_eq!(annotation_to_type("int | str"), Type::Unknown);
        assert_eq!(annotation_to_type("requests.Session"), Type::Unknown);
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
            cwd: tmp.path().to_path_buf(),
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
            cwd: tmp.path().to_path_buf(),
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
            }],
        };
        let shapes = shapes_from_introspected(&intro);
        assert!(shapes.class_shapes.is_empty());
    }
}
