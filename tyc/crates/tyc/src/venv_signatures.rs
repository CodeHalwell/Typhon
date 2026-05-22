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
//! `enrich_project_shapes_with_venv` is the CLI entry point: it walks
//! every `.ty` file's import statements, looks up each dotted module
//! that isn't already a known project module, and folds the
//! introspection result into the project shape registry that the
//! checker consumes.

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
}

impl VenvSignatures {
    /// Discover the venv's Python (or `python3` on PATH) and build an
    /// empty cache. `allowed_top_level` is the set of import names
    /// the caller is willing to introspect — typically the project's
    /// declared dependencies' top-level packages.
    pub fn for_project_root(project_root: &Path, allowed_top_level: HashSet<String>) -> Self {
        let venv_python = project_root.join(".venv").join("bin").join("python");
        let python_bin = if venv_python.is_file() {
            Some(venv_python)
        } else {
            which_python3()
        };
        Self {
            python_bin,
            cwd: project_root.to_path_buf(),
            allowed_top_level,
            cache: HashMap::new(),
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
                    let shapes = results.remove(name.as_str()).map(|intro| shapes_from_introspected(&intro));
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
                        &self.cwd,
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
/// from sibling modules.
#[cfg(test)]
pub fn which_python3_for_test() -> Option<PathBuf> {
    which_python3()
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
        })
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
        kind = kind_of(obj)
        members.append({
            "name": name,
            "kind": kind,
            "params": params_of(obj) if kind in ("class", "function") else None,
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
    let mut child = cmd
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
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
                if let Some(info) = arity_info_from_params(params) {
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
        fields.insert(p.name.clone(), Type::Unknown);
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
    })
}

/// Build an [`ArityInfo`] for an introspected free function.
fn arity_info_from_params(params: &[IntrospectedParam]) -> Option<ArityInfo> {
    let mut param_names: Vec<String> = Vec::new();
    let mut required_positional: Vec<bool> = Vec::new();
    let mut kwonly_names: Vec<String> = Vec::new();
    let mut kwonly_required: Vec<String> = Vec::new();
    let mut has_kwarg = false;
    let mut has_vararg = false;
    for p in params {
        match p.kind.as_str() {
            "var_positional" => has_vararg = true,
            "var_keyword" => has_kwarg = true,
            "keyword_only" => {
                kwonly_names.push(p.name.clone());
                if !p.has_default {
                    kwonly_required.push(p.name.clone());
                }
            }
            _ => {
                param_names.push(p.name.clone());
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
    Some(ArityInfo {
        param_names,
        min_positional,
        required_positional,
        max_positional,
        kwonly_names,
        kwonly_required,
        has_kwarg,
        vararg_type: None,
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
) {
    if allowed_top_level.is_empty() {
        return;
    }
    let mut cache = VenvSignatures::for_project_root(project_root, allowed_top_level);
    if cache.python_bin.is_none() {
        return;
    }
    let imports = collect_imported_modules(paths);
    // Filter once, then batch the whole list into a single subprocess
    // (see `VenvSignatures::preload`). The earlier per-module loop
    // dominated `tyc check` time on projects with many declared deps —
    // each `import requests.X` was costing a fresh Python startup.
    let needed: Vec<String> = imports
        .iter()
        .filter(|m| !project_shapes.contains_key(*m) && !project_module_set.contains(*m))
        .cloned()
        .collect();
    cache.preload(&needed);
    for module in needed {
        if let Some(shapes) = cache.module_shapes(&module) {
            project_shapes.insert(module, shapes.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str, kind: &str, has_default: bool) -> IntrospectedParam {
        IntrospectedParam {
            name: name.into(),
            kind: kind.into(),
            has_default,
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
        let info = arity_info_from_params(&params).expect("info");
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
        let info = arity_info_from_params(&params).expect("info");
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
                },
                IntrospectedMember {
                    name: "Agent".into(),
                    kind: "class".into(),
                    params: Some(vec![p("name", "keyword_only", false)]),
                },
            ],
        };
        let shapes = shapes_from_introspected(&intro);
        assert!(shapes.class_shapes.contains_key("Agent"));
        assert!(!shapes.class_shapes.contains_key("VERSION"));
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

        let allowed: HashSet<String> =
            ["pkg_a", "pkg_b", "pkg_c", "pkg_d", "pkg_e"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
        let mut cache = VenvSignatures {
            python_bin: Some(stub),
            cwd: tmp.path().to_path_buf(),
            allowed_top_level: allowed,
            cache: HashMap::new(),
        };
        let modules: Vec<String> =
            ["pkg_a", "pkg_b", "pkg_c", "pkg_d", "pkg_e"]
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
        };
        let modules: Vec<String> =
            ["pkg_a", "pkg_b", "pkg_c"].iter().map(|s| (*s).to_owned()).collect();
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
            }],
        };
        let shapes = shapes_from_introspected(&intro);
        assert!(shapes.class_shapes.is_empty());
    }
}
