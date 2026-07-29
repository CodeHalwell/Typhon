//! `typhon.toml` configuration file parsing.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The contents of a `typhon.toml` project file.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TyphonConfig {
    pub project: ProjectConfig,
    pub python: PythonConfig,
    pub emit: EmitConfig,
    pub strictness: StrictnessConfig,
    /// `[optimise]` — project-wide optimisation level. Level 1 flips the
    /// default of the four optimisation strictness knobs (`auto-memoise`,
    /// `auto-gather`, `auto-parallel`, `pgo-memoise`) to `true`; an explicit
    /// `[strictness]` entry for any of them always wins. Resolved after load
    /// (and after a CLI `-O`/`--optimise`) via [`TyphonConfig::resolve_optimise`].
    #[serde(default)]
    pub optimise: OptimiseConfig,
    pub env: EnvConfig,
    #[serde(default)]
    pub checker: CheckerConfig,
    /// Runtime Python dependencies, keyed by package name. The value is a
    /// PEP 440 version specifier (e.g. `"^2.0"`, `">=1.0,<2"`, `"*"`).
    /// Empty when typhon.toml does not declare any dependencies — projects
    /// can still manage them through `uv`/`pip` directly without using the
    /// `tyc add` / `tyc sync` commands.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, String>,
    /// Development-only dependencies (test runners, linters, etc.). Not
    /// installed for downstream consumers of the package.
    #[serde(
        default,
        rename = "dev-dependencies",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub dev_dependencies: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectConfig {
    pub name: String,
    pub version: String,
    pub src: String,
    pub out: String,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            version: "0.1.0".into(),
            src: "src".into(),
            out: "build".into(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct PythonConfig {
    /// Target Python version, e.g. `"3.13"`.
    pub target: String,
    /// Enable free-threaded Python (3.13t / 3.14t).
    pub free_threaded: bool,
}

impl Default for PythonConfig {
    fn default() -> Self {
        Self {
            target: "3.13".into(),
            free_threaded: false,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct EmitConfig {
    /// Default class emission target. Only `"dataclass"` (the default) is
    /// implemented; a project-wide `"pydantic"` default is rejected by
    /// [`TyphonConfig::validate`] (use the per-class `model` keyword instead).
    pub class_default: String,
    /// Post-process emitted Python through ruff format.
    pub format: bool,
    /// List of additional base-class names that, when used as a base class,
    /// suppress the automatic `@dataclasses.dataclass(slots=True)` decorator.
    /// Matched by last segment.
    #[serde(default, rename = "skip-decoration-bases")]
    pub skip_decoration_bases: Vec<String>,
    /// Value passed to Pydantic's `ConfigDict(extra=…)` for every `model`
    /// class.  Accepted values are `"forbid"` (default — reject unexpected
    /// fields at runtime), `"ignore"` (silently drop them), and `"allow"`
    /// (pass them through as extra attributes).  Unknown values are rejected
    /// by [`TyphonConfig::validate`] with `ConfigError::InvalidModelExtra`.
    #[serde(default = "default_model_extra", rename = "model-extra")]
    pub model_extra: String,
    /// When `true`, inject a `typhon_runtime.traceback.install()` call into
    /// the entry module's `if __name__ == "__main__":` block so an uncaught
    /// exception's traceback is rewritten to `.ty` source automatically (the
    /// same mapping `tyc trace` applies). Defaults to `false` so existing
    /// projects are unaffected and runtime-free entry points stay
    /// dependency-free.
    #[serde(default, rename = "traceback-remap")]
    pub traceback_remap: bool,
}

impl Default for EmitConfig {
    fn default() -> Self {
        Self {
            class_default: "dataclass".into(),
            format: true,
            skip_decoration_bases: Vec::new(),
            model_extra: "forbid".into(),
            traceback_remap: false,
        }
    }
}

/// Accepted values for `[emit] class-default`. Anything else is rejected by
/// [`TyphonConfig::validate`] — including the empty string and historical
/// aliases like `"struct"` / `"regular"` / `"none"` which were never wired
/// through the emitter.
// `"pydantic"` is intentionally NOT here: a project-wide pydantic default is
// not wired into the emitter, so it is rejected with a dedicated message in
// [`TyphonConfig::validate`] pointing at the per-class `model` keyword.
pub const ALLOWED_CLASS_DEFAULTS: &[&str] = &["dataclass"];

/// Accepted values for `[emit] model-extra`. Anything else is rejected by
/// [`TyphonConfig::validate`].
pub const ALLOWED_MODEL_EXTRAS: &[&str] = &["forbid", "ignore", "allow"];

/// Accepted values for `[checker] external`. `"none"` disables the external
/// pass; `"ty"` runs Astral's `ty`. Anything else (e.g. `"mypy"`, `"pyright"`)
/// is rejected by [`TyphonConfig::validate`] rather than silently ignored —
/// a user who set `external = "mypy"` expecting a second checker would
/// otherwise get no external checking at all with no indication why.
pub const ALLOWED_CHECKERS: &[&str] = &["none", "ty"];

fn default_model_extra() -> String {
    "forbid".into()
}

fn default_stub_check() -> String {
    "error".into()
}

/// `[optimise]` — project-wide optimisation level.
///
/// `level = 0` (the default) leaves every optimisation knob at its own
/// default. `level = 1` flips the *default* of `auto-memoise`, `auto-gather`,
/// `auto-parallel`, and `pgo-memoise` to `true`. An explicit `[strictness]`
/// entry for any of those four always wins over the level-derived default,
/// so a user can set `level = 1` and still write `auto-parallel = false` to
/// keep that one knob off. Only `0` and `1` are accepted — any other integer
/// is rejected by [`TyphonConfig::validate`] with
/// [`ConfigError::InvalidOptimiseLevel`], and a non-integer value is rejected
/// at parse time.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct OptimiseConfig {
    /// Optimisation level: `0` (default) or `1`.
    pub level: u8,
}

/// Accepted values for `[strictness] parallel-backend`. `"threads"` (the
/// default) runs auto-parallel maps on a `ThreadPoolExecutor`; `"interpreters"`
/// targets PEP 734 sub-interpreters. Anything else is rejected by
/// [`TyphonConfig::validate`] — mirroring how `[checker] external` is validated
/// — rather than silently falling back to threads.
pub const ALLOWED_PARALLEL_BACKENDS: &[&str] = &["threads", "interpreters"];

#[derive(Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct StrictnessConfig {
    pub no_implicit_any: bool,
    pub unused_import: String,
    pub exhaustive_match: String,
    /// Severity for `tyc::method_in_class_body` (Rule 4: methods live in
    /// `impl Name:`, not in the class body). `"warn"` (default) is the
    /// shipped behaviour and matches every other "nudge" diagnostic.
    /// `"error"` promotes it through [`crate::commands::util::apply_strictness`]
    /// so CI breaks on the form. `"off"` suppresses the diagnostic
    /// entirely — useful for codebases still mid-migration.
    pub methods_in_class_body: String,
    /// Severity for the *attribute-rooted* form of `tyc::nullable_use` —
    /// dereferencing a possibly-`None` field (`self.db.host` where `db: Db?`).
    /// `"warn"` (default) is the v1.0.0-alpha.7 introduction severity: the
    /// check never ran before that release, so an immediate error would
    /// reject programs whose nullable field happens always to be populated.
    /// `"error"` promotes it (the documented migration path); `"off"`
    /// suppresses it. The *bare-name* form (`x.upper()` where `x: str?`) has
    /// always been an error and is not governed by this knob.
    pub nullable_use: String,
    /// When true, every function that passes the six-condition purity check
    /// is treated as if the user had written `@memo` — the desugarer emits a
    /// `@functools.cache` decorator. Off by default; opt in per-project.
    ///
    /// `Option<bool>` to distinguish "absent" from "explicitly set": `None`
    /// takes the [`OptimiseConfig`]-derived default (off at level 0, on at
    /// level 1), while `Some(v)` is an explicit toml entry that always wins.
    /// [`TyphonConfig::resolve_optimise`] collapses this to a concrete value
    /// after load; read it with `.unwrap_or(false)`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_memoise: Option<bool>,
    /// When true, runs of two-or-more consecutive independent `await` calls
    /// inside an `async def` are rewritten into an `asyncio.TaskGroup` block
    /// so they execute concurrently. Independence is determined by static
    /// data-flow on bound names; a run is only folded when every callee is a
    /// `@gatherable` `async def` — declared in the same module **or imported
    /// from another project module** (the cross-module eligibility set is
    /// seeded from each module's published `@gatherable` names). Off by
    /// default — flip on per-project to apply the rewrite globally. (Phase 4
    /// auto-gather inference; explicit `gather:` blocks are unaffected.)
    ///
    /// `Option<bool>`: see [`StrictnessConfig::auto_memoise`] — `None` takes
    /// the optimise-level default, `Some(v)` is an explicit override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_gather: Option<bool>,
    /// When true (the default), `tyc check` and `tyc build` surface a
    /// `tyc::gather_opportunity` advice for every run of 2+ adjacent
    /// independent awaited calls inside an `async def` — including awaited
    /// method calls on imported clients, which `auto-gather` never touches —
    /// and the language server shows the same hint live in the editor.
    /// Advice-only: it suggests wrapping the run in an explicit `gather:`
    /// block but never rewrites (concurrency is a behaviour change the author
    /// opts into). Set `false` to silence the nudge; it never blocks a build
    /// regardless.
    pub suggest_gather: bool,
    /// When true, `tyc build` consults `typhon-profile.json` (produced by a
    /// prior `tyc profile` run) and promotes every pure function whose call
    /// count meets [`StrictnessConfig::pgo_min_calls`] to `@functools.cache`,
    /// even if the user did not write `@memo`. Off by default. (Phase 4
    /// profile-guided optimisation; complements `auto-memoise` which acts
    /// on every pure function regardless of profile data.)
    ///
    /// `Option<bool>`: see [`StrictnessConfig::auto_memoise`] — `None` takes
    /// the optimise-level default, `Some(v)` is an explicit override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pgo_memoise: Option<bool>,
    /// Minimum observed call count for a function to be promoted by
    /// [`StrictnessConfig::pgo_memoise`]. Defaults to 100 — high enough
    /// that one-off entry points stay un-cached, low enough that an
    /// inner-loop helper qualifies after a single representative run.
    pub pgo_min_calls: u64,
    /// When true, list comprehensions whose element is a pure call (no
    /// I/O, no module-state writes, no entropy/clock) are rewritten at
    /// build time into a thread-pool map.  Combine with
    /// [`PythonConfig::free_threaded`] to release the GIL across workers
    /// and get real parallelism; on a stock CPython the rewrite still
    /// runs but the GIL serialises the workers.  Off by default.
    ///
    /// `Option<bool>`: see [`StrictnessConfig::auto_memoise`] — `None` takes
    /// the optimise-level default, `Some(v)` is an explicit override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_parallel: Option<bool>,
    /// Minimum statically-detectable iterable length for a comprehension
    /// to qualify for parallelisation.  Below this threshold the
    /// thread-pool overhead exceeds any wins, so the rewrite is skipped.
    /// Default 64.  When the iterable size cannot be inferred (e.g. an
    /// arbitrary function call), the threshold is treated as zero —
    /// users opting into `auto-parallel` accept that contract.
    pub parallel_min_size: u64,
    /// Severity for `tyc::resource_not_managed`. A call to a known
    /// resource-returning function (`open`, `socket.socket`, …) bound
    /// to a variable outside a `with` statement leaves cleanup at
    /// the mercy of the garbage collector. `"warn"` (default) keeps
    /// the diagnostic visible without breaking CI; `"error"` promotes
    /// it for codebases that have already paid down the migration
    /// cost; `"off"` drops the diagnostic entirely.
    pub require_with: String,
    /// Severity for `tyc::blocking_in_async`. A direct call to a
    /// known-blocking stdlib function (`time.sleep`, `requests.get`,
    /// `socket.recv`, `subprocess.run`, …) from inside an `async def`
    /// halts the event loop. `"warn"` (default) surfaces the issue
    /// without breaking CI; `"error"` promotes for codebases that
    /// have finished migrating to async-aware libraries (`httpx`
    /// instead of `requests`, `aiofiles` instead of bare `open`,
    /// `asyncio.sleep` instead of `time.sleep`); `"off"` suppresses.
    pub blocking_in_async: String,
    /// When true, silence the warning for `tyc::contains_secret_literal`
    /// which checks for sensitive environmental variables mapped using
    /// `comptime let API_KEY = env("API_KEY")`.
    pub allow_secret_comptime: bool,
    /// Severity for `tyc::stub_mismatch` produced by `tyc check --stubs`.
    /// When a `.dty` stub's surface API diverges from its sibling `.ty`/`.py`
    /// implementation, this diagnostic is emitted.
    /// - `"error"` (default) — stub drift fails the check; suitable for
    ///   projects that version-control their stubs alongside their code.
    /// - `"warn"` — drift is surfaced but does not break CI; useful while
    ///   a migration is in progress.
    /// - `"off"` — stubs are checked but mismatches are silently dropped;
    ///   use only when `--stubs` is run opportunistically.
    #[serde(default = "default_stub_check")]
    pub stub_check: String,
    /// Severity when a declared dependency is imported but could not be
    /// introspected for signatures (no reachable `.venv`/`python3`, or the
    /// package isn't installed) — so its third-party arity/type checks are
    /// silently skipped. `"warn"` (default) surfaces it, `"error"` fails the
    /// build/check (CI-gating), `"off"` restores the old silent behaviour.
    pub unintrospectable_dependency: String,
    /// When true (the default), the compiler surfaces the `tyc::perf_*`
    /// advice-lint family — micro-optimisation nudges (redundant work in a
    /// hot path, an avoidable allocation, …). Advice-only; never blocks a
    /// build. Set `false` to silence the whole family. (No consumer yet —
    /// parsed and reachable for the wave that lands the lints.)
    pub suggest_perf: bool,
    /// When true (the default), the compiler surfaces `tyc::parallel_opportunity`
    /// advice — a loop or comprehension that could be parallelised (see
    /// `auto-parallel`). Advice-only; never blocks a build. Set `false` to
    /// silence it. (No consumer yet — parsed and reachable for the wave that
    /// lands the lint.)
    pub suggest_parallel: bool,
    /// When true, integer accumulator loops (`for x in xs: total += f(x)`)
    /// over a pure body are eligible for parallel reduction. Off by default.
    /// **Requires `auto-parallel` to have any effect** — it gates the same
    /// build-time rewrite pass, extending it from pure comprehensions to
    /// accumulator loops. (No consumer yet — parsed and reachable for the
    /// wave that lands the rewrite.)
    pub auto_parallel_reductions: bool,
    /// Execution backend for auto-parallel rewrites. `"threads"` (the
    /// default) uses a `concurrent.futures.ThreadPoolExecutor`;
    /// `"interpreters"` targets PEP 734 sub-interpreters. Validated against
    /// [`ALLOWED_PARALLEL_BACKENDS`] — an unknown value is rejected at config
    /// load rather than silently falling back to threads. (No consumer yet —
    /// parsed and reachable for the wave that threads it through the rewrite.)
    pub parallel_backend: String,
}

impl Default for StrictnessConfig {
    fn default() -> Self {
        Self {
            no_implicit_any: true,
            unused_import: "warn".into(),
            exhaustive_match: "error".into(),
            methods_in_class_body: "warn".into(),
            nullable_use: "warn".into(),
            // `None` = "absent from toml": resolved to a concrete bool by
            // `TyphonConfig::resolve_optimise` (off at optimise level 0, on at
            // level 1). An explicit toml entry deserialises to `Some(v)` and
            // always wins.
            auto_memoise: None,
            auto_gather: None,
            suggest_gather: true,
            pgo_memoise: None,
            pgo_min_calls: 100,
            auto_parallel: None,
            parallel_min_size: 64,
            require_with: "warn".into(),
            blocking_in_async: "warn".into(),
            allow_secret_comptime: false,
            stub_check: "error".into(),
            unintrospectable_dependency: "warn".into(),
            suggest_perf: true,
            suggest_parallel: true,
            auto_parallel_reductions: false,
            parallel_backend: "threads".into(),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EnvConfig {
    /// Env vars that must be resolvable at build time for `comptime env()`.
    pub required: Vec<String>,
}

/// `[checker]` — second-stage type-checking over the emitted Python.
///
/// `tyc-types` enforces Typhon-specific semantics (let/mut, sealed unions,
/// `Result`/`?`, interface conformance, …). An external checker complements
/// it by validating the standard Python typing spec against **typeshed** —
/// the only path that covers C-extension libraries (numpy / pandas / torch)
/// and typeshed-only annotations that runtime venv introspection cannot see.
/// See `docs/ty-integration.md`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct CheckerConfig {
    /// External checker to run after a successful build/check. `"none"`
    /// (default) disables it; `"ty"` runs Astral's `ty check` over the
    /// emitted Python and re-attributes its diagnostics back to the `.ty`
    /// source via the `.py.map` sidecars.
    pub external: String,
    /// Extra arguments forwarded verbatim to the external checker.
    pub external_args: Vec<String>,
}

impl Default for CheckerConfig {
    fn default() -> Self {
        Self {
            external: "none".into(),
            external_args: Vec::new(),
        }
    }
}

impl TyphonConfig {
    /// Load `typhon.toml` from the given directory (or any ancestor).
    ///
    /// Returns:
    /// - `Ok(Some((path, config)))` when a config file is found and parsed.
    /// - `Ok(None)` when no `typhon.toml` is found in the directory tree.
    /// - `Err(e)` when a `typhon.toml` is found but cannot be read or parsed.
    pub fn load(start: &Path) -> Result<Option<(PathBuf, Self)>, crate::config::ConfigError> {
        let mut dir = start.to_path_buf();
        loop {
            let candidate = dir.join("typhon.toml");
            if candidate.exists() {
                let text = std::fs::read_to_string(&candidate).map_err(|e| {
                    crate::config::ConfigError::Io {
                        path: candidate.to_string_lossy().into_owned(),
                        cause: e.to_string(),
                    }
                })?;
                let config: Self =
                    toml::from_str(&text).map_err(|e| crate::config::ConfigError::Parse {
                        path: candidate.to_string_lossy().into_owned(),
                        cause: e.to_string(),
                    })?;
                config.validate(&candidate)?;
                return Ok(Some((candidate, config)));
            }
            if !dir.pop() {
                break;
            }
        }
        Ok(None)
    }

    /// Serialize this config back to TOML.
    ///
    /// Returns an error if serialization fails (should be unreachable in
    /// practice, but returning `Result` avoids silent data loss).
    pub fn to_toml_string(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    /// Reject any configuration that violates a hard project-wide
    /// invariant. Today the only such invariant is the Python target:
    /// Typhon requires CPython **3.13+** because the emit pipeline
    /// depends on PEP 695 type-parameter syntax, PEP 692 `**kwargs`,
    /// and `asyncio.TaskGroup` — all 3.11/3.12-or-later, with several
    /// features (free-threaded mode, PEP 750 t-strings) gated on 3.13t
    /// specifically. Older targets are not supported and would
    /// silently emit code the user's runtime refuses.
    ///
    /// Accepts the bare-major form (`"3.13"`, `"3.14"`, `"3.15"`) and the
    /// free-threaded suffix (`"3.13t"`, `"3.14t"`, `"3.15t"`). Anything that
    /// parses below 3.13 — including the still-supported-by-CPython
    /// 3.11 and 3.12 — is a hard error pointing at the offending
    /// `typhon.toml`.
    ///
    /// Also validates `[optimise] level` (only `0` / `1`) and
    /// `[strictness] parallel-backend` (only `"threads"` / `"interpreters"`).
    pub fn validate(&self, source_path: &Path) -> Result<(), ConfigError> {
        let raw = self.python.target.trim();
        let (major, minor) =
            parse_python_target(raw).ok_or_else(|| ConfigError::UnsupportedPythonTarget {
                path: source_path.to_string_lossy().into_owned(),
                target: raw.to_owned(),
                reason: "expected a `MAJOR.MINOR` string such as `3.13` or `3.14t`".to_owned(),
            })?;
        if (major, minor) < (3, 13) {
            return Err(ConfigError::UnsupportedPythonTarget {
                path: source_path.to_string_lossy().into_owned(),
                target: raw.to_owned(),
                reason: format!(
                    "Typhon requires CPython 3.13+ (got {major}.{minor}); update `[python] target` to `\"3.13\"` or newer",
                ),
            });
        }
        // Reject `[emit] class-default = "..."` outside the allow-list.
        // Empty strings, typos, and removed-aliases (`"struct"`,
        // `"regular"`, `"none"`) all surface here rather than silently
        // falling back to dataclass at emit time.
        let cd = self.emit.class_default.trim();
        if cd == "pydantic" {
            // Project-wide pydantic default is not wired into the emitter
            // (it was silently a no-op — every `class` still emitted a
            // dataclass). Reject it explicitly and point at the per-class
            // `model` keyword, which IS implemented, rather than letting the
            // user believe the knob took effect.
            return Err(ConfigError::InvalidClassDefault {
                path: source_path.to_string_lossy().into_owned(),
                value: self.emit.class_default.clone(),
                allowed:
                    "dataclass (a project-wide pydantic default is not yet implemented — declare \
                     a boundary type with the `model` keyword per class instead)"
                        .to_owned(),
            });
        }
        if !ALLOWED_CLASS_DEFAULTS.contains(&cd) {
            return Err(ConfigError::InvalidClassDefault {
                path: source_path.to_string_lossy().into_owned(),
                value: self.emit.class_default.clone(),
                allowed: ALLOWED_CLASS_DEFAULTS.join(", "),
            });
        }
        // Reject `[emit] model-extra = "..."` outside the allow-list.
        let me = self.emit.model_extra.trim();
        if !ALLOWED_MODEL_EXTRAS.contains(&me) {
            return Err(ConfigError::InvalidModelExtra {
                path: source_path.to_string_lossy().into_owned(),
                value: self.emit.model_extra.clone(),
                allowed: ALLOWED_MODEL_EXTRAS.join(", "),
            });
        }
        // Reject an unknown `[strictness]` severity string. These take
        // `"off"` / `"warn"` / `"error"`; a typo (`"eror"`) or wrong case
        // (`"WARN"`) used to be silently ignored, reverting to the default —
        // so a user who believed they had CI-gated a check actually had it
        // off. Surface it instead.
        const ALLOWED_SEVERITIES: [&str; 3] = ["off", "warn", "error"];
        let severities = [
            ("unused-import", &self.strictness.unused_import),
            ("exhaustive-match", &self.strictness.exhaustive_match),
            (
                "methods-in-class-body",
                &self.strictness.methods_in_class_body,
            ),
            ("nullable-use", &self.strictness.nullable_use),
            ("require-with", &self.strictness.require_with),
            ("blocking-in-async", &self.strictness.blocking_in_async),
            ("stub-check", &self.strictness.stub_check),
            (
                "unintrospectable-dependency",
                &self.strictness.unintrospectable_dependency,
            ),
        ];
        for (key, value) in severities {
            if !ALLOWED_SEVERITIES.contains(&value.trim()) {
                return Err(ConfigError::InvalidSeverity {
                    path: source_path.to_string_lossy().into_owned(),
                    key: key.to_owned(),
                    value: value.clone(),
                    allowed: ALLOWED_SEVERITIES.join(", "),
                });
            }
        }
        // Reject an unknown `[checker] external`. Only `"none"` and `"ty"` are
        // wired; a typo or an unsupported checker name (`"mypy"`) used to be
        // accepted and then silently do nothing.
        let ext = self.checker.external.trim();
        if !ALLOWED_CHECKERS.contains(&ext) {
            return Err(ConfigError::InvalidChecker {
                path: source_path.to_string_lossy().into_owned(),
                value: self.checker.external.clone(),
                allowed: ALLOWED_CHECKERS.join(", "),
            });
        }
        // Reject an `[optimise] level` outside `0` / `1`. A non-integer value
        // (`level = "1"`, `level = 1.5`) is already rejected at parse time by
        // the `u8` deserialisation; here we catch an in-range-for-`u8` but
        // out-of-range-for-the-feature integer like `2`.
        if self.optimise.level > 1 {
            return Err(ConfigError::InvalidOptimiseLevel {
                path: source_path.to_string_lossy().into_owned(),
                value: self.optimise.level,
            });
        }
        // Reject an unknown `[strictness] parallel-backend`. Only `"threads"`
        // and `"interpreters"` are wired; a typo used to silently fall back to
        // threads.
        let backend = self.strictness.parallel_backend.trim();
        if !ALLOWED_PARALLEL_BACKENDS.contains(&backend) {
            return Err(ConfigError::InvalidParallelBackend {
                path: source_path.to_string_lossy().into_owned(),
                value: self.strictness.parallel_backend.clone(),
                allowed: ALLOWED_PARALLEL_BACKENDS.join(", "),
            });
        }
        Ok(())
    }

    /// Resolve the four optimise-gated strictness knobs (`auto-memoise`,
    /// `auto-gather`, `auto-parallel`, `pgo-memoise`) to concrete values,
    /// honouring `[optimise] level` and an optional CLI `-O`/`--optimise`
    /// override (`tyc build` only).
    ///
    /// Level 1 (from either the toml or `cli_force_level1`) flips each knob's
    /// default to `true`; an explicit `[strictness]` entry — deserialised as
    /// `Some(v)` — always wins over the level-derived default. After this
    /// call every one of the four `Option<bool>` fields is `Some(_)`, so
    /// consumers read them with `.unwrap_or(false)`. Idempotent.
    pub fn resolve_optimise(&mut self, cli_force_level1: bool) {
        let level1 = cli_force_level1 || self.optimise.level >= 1;
        let s = &mut self.strictness;
        s.auto_memoise = Some(s.auto_memoise.unwrap_or(level1));
        s.auto_gather = Some(s.auto_gather.unwrap_or(level1));
        s.auto_parallel = Some(s.auto_parallel.unwrap_or(level1));
        s.pgo_memoise = Some(s.pgo_memoise.unwrap_or(level1));
    }
}

/// Parse a `[python] target` value into `(major, minor)`. Accepts the
/// bare form (`"3.13"`, `"3.14"`, `"3.15"`), the free-threaded suffix
/// (`"3.13t"` … `"3.15t"`), and tolerates patch-level strings like
/// `"3.15.2"` by ignoring everything past the second segment. Returns
/// `None` for malformed input.
///
/// Exposed at `pub(crate)` so `tyc build` can gate PEP 810 native
/// lazy-import lowering on a `(major, minor) >= (3, 15)` target without
/// duplicating the suffix-tolerant parse. The `(major, minor)` tuple —
/// rather than the minor-only [`parse_python_minor`] in `build.rs` — is
/// the correct comparison basis: a hypothetical future `"4.0"` compares
/// `>= (3, 15)` as a version, whereas its minor `0` would not.
pub(crate) fn parse_python_target(s: &str) -> Option<(u32, u32)> {
    let mut parts = s.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor_raw = parts.next()?;
    // Trim the `t` (free-threaded) or `rc1` / `a1` style suffix — only
    // the leading digits matter for the floor check.
    let minor_digits: String = minor_raw
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if minor_digits.is_empty() {
        return None;
    }
    let minor: u32 = minor_digits.parse().ok()?;
    Some((major, minor))
}

/// Errors that can occur when loading a `typhon.toml` file.
#[derive(Debug)]
pub enum ConfigError {
    Io {
        path: String,
        cause: String,
    },
    Parse {
        path: String,
        cause: String,
    },
    /// `[python] target` is missing, malformed, or below the
    /// project-wide 3.13 floor. Emitted by [`TyphonConfig::validate`].
    UnsupportedPythonTarget {
        path: String,
        target: String,
        reason: String,
    },
    /// `[emit] class-default` is set to a value outside
    /// [`ALLOWED_CLASS_DEFAULTS`]. Emitted by [`TyphonConfig::validate`].
    InvalidClassDefault {
        path: String,
        value: String,
        allowed: String,
    },
    /// `[emit] model-extra` is set to a value outside
    /// [`ALLOWED_MODEL_EXTRAS`]. Emitted by [`TyphonConfig::validate`].
    InvalidModelExtra {
        path: String,
        value: String,
        allowed: String,
    },
    InvalidSeverity {
        path: String,
        key: String,
        value: String,
        allowed: String,
    },
    /// `[checker] external` is set to a value outside [`ALLOWED_CHECKERS`].
    /// Emitted by [`TyphonConfig::validate`].
    InvalidChecker {
        path: String,
        value: String,
        allowed: String,
    },
    /// `[optimise] level` is an integer outside `0` / `1`. Emitted by
    /// [`TyphonConfig::validate`].
    InvalidOptimiseLevel {
        path: String,
        value: u8,
    },
    /// `[strictness] parallel-backend` is set to a value outside
    /// [`ALLOWED_PARALLEL_BACKENDS`]. Emitted by [`TyphonConfig::validate`].
    InvalidParallelBackend {
        path: String,
        value: String,
        allowed: String,
    },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io { path, cause } => {
                write!(f, "cannot read '{}': {}", path, cause)
            }
            ConfigError::Parse { path, cause } => {
                write!(f, "invalid typhon.toml '{}': {}", path, cause)
            }
            ConfigError::UnsupportedPythonTarget {
                path,
                target,
                reason,
            } => {
                write!(
                    f,
                    "unsupported `[python] target = \"{target}\"` in '{path}': {reason}",
                )
            }
            ConfigError::InvalidClassDefault {
                path,
                value,
                allowed,
            } => {
                write!(
                    f,
                    "invalid `[emit] class-default = \"{value}\"` in '{path}': allowed values are {allowed}",
                )
            }
            ConfigError::InvalidModelExtra {
                path,
                value,
                allowed,
            } => {
                write!(
                    f,
                    "invalid `[emit] model-extra = \"{value}\"` in '{path}': allowed values are {allowed}",
                )
            }
            ConfigError::InvalidSeverity {
                path,
                key,
                value,
                allowed,
            } => {
                write!(
                    f,
                    "invalid `[strictness] {key} = \"{value}\"` in '{path}': allowed values are {allowed}",
                )
            }
            ConfigError::InvalidChecker {
                path,
                value,
                allowed,
            } => {
                write!(
                    f,
                    "invalid `[checker] external = \"{value}\"` in '{path}': allowed values are {allowed}",
                )
            }
            ConfigError::InvalidOptimiseLevel { path, value } => {
                write!(
                    f,
                    "invalid `[optimise] level = {value}` in '{path}': allowed values are 0, 1",
                )
            }
            ConfigError::InvalidParallelBackend {
                path,
                value,
                allowed,
            } => {
                write!(
                    f,
                    "invalid `[strictness] parallel-backend = \"{value}\"` in '{path}': allowed values are {allowed}",
                )
            }
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_target(t: &str) -> TyphonConfig {
        TyphonConfig {
            python: PythonConfig {
                target: t.into(),
                free_threaded: false,
            },
            ..Default::default()
        }
    }

    #[test]
    fn checker_config_defaults_to_none() {
        let cfg = TyphonConfig::default();
        assert_eq!(cfg.checker.external, "none");
        assert!(cfg.checker.external_args.is_empty());
    }

    #[test]
    fn checker_config_parses_external_ty() {
        let cfg: TyphonConfig = toml::from_str(
            "[project]\nname = \"x\"\n\n[checker]\nexternal = \"ty\"\nexternal-args = [\"--error-on-warning\"]\n",
        )
        .expect("parse");
        assert_eq!(cfg.checker.external, "ty");
        assert_eq!(cfg.checker.external_args, vec!["--error-on-warning"]);
    }

    #[test]
    fn validate_accepts_supported_targets() {
        let path = Path::new("typhon.toml");
        for t in &[
            "3.13", "3.13t", "3.14", "3.14t", "3.15", "3.15t", "3.13.2", "3.15.2",
        ] {
            cfg_with_target(t)
                .validate(path)
                .unwrap_or_else(|e| panic!("target {t} should be accepted, got {e}"));
        }
    }

    #[test]
    fn validate_rejects_below_3_13() {
        let path = Path::new("typhon.toml");
        for t in &["3.12", "3.11", "3.10", "3.9", "2.7"] {
            let err = cfg_with_target(t)
                .validate(path)
                .expect_err(&format!("target {t} should be rejected"));
            match err {
                ConfigError::UnsupportedPythonTarget { reason, .. } => {
                    assert!(
                        reason.contains("3.13+"),
                        "expected 3.13+ hint, got {reason}"
                    );
                }
                _ => panic!("expected UnsupportedPythonTarget for {t}, got {err:?}"),
            }
        }
    }

    #[test]
    fn validate_rejects_malformed_target() {
        let path = Path::new("typhon.toml");
        for t in &["", "abc", "3", "..", "x.y"] {
            let err = cfg_with_target(t)
                .validate(path)
                .expect_err(&format!("target {t:?} should be rejected"));
            assert!(matches!(err, ConfigError::UnsupportedPythonTarget { .. }));
        }
    }

    #[test]
    fn validate_accepts_dataclass() {
        let path = Path::new("typhon.toml");
        let mut cfg = cfg_with_target("3.13");
        cfg.emit.class_default = "dataclass".into();
        cfg.validate(path)
            .unwrap_or_else(|e| panic!("class-default dataclass should be accepted, got {e}"));
    }

    #[test]
    fn validate_rejects_pydantic_class_default_with_model_hint() {
        // A project-wide pydantic default is not wired; it must be rejected
        // (not a silent no-op) with a pointer to the per-class `model` keyword.
        let path = Path::new("typhon.toml");
        let mut cfg = cfg_with_target("3.13");
        cfg.emit.class_default = "pydantic".into();
        let err = cfg
            .validate(path)
            .expect_err("class-default pydantic should be rejected");
        match err {
            ConfigError::InvalidClassDefault { value, allowed, .. } => {
                assert_eq!(value, "pydantic");
                assert!(
                    allowed.contains("model"),
                    "message should mention `model`, got {allowed}"
                );
            }
            other => panic!("expected InvalidClassDefault, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_plain_regular_struct_none() {
        let path = Path::new("typhon.toml");
        for v in &["", "regular", "struct", "none", "plain"] {
            let mut cfg = cfg_with_target("3.13");
            cfg.emit.class_default = (*v).into();
            let err = cfg
                .validate(path)
                .expect_err(&format!("class-default {v:?} should be rejected"));
            match err {
                ConfigError::InvalidClassDefault { value, allowed, .. } => {
                    assert_eq!(value, *v);
                    assert!(
                        allowed.contains("dataclass"),
                        "expected allowed list to mention dataclass, got {allowed}"
                    );
                }
                other => panic!("expected InvalidClassDefault for {v:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn validate_rejects_unknown_class_default() {
        let path = Path::new("typhon.toml");
        let mut cfg = cfg_with_target("3.13");
        cfg.emit.class_default = "msgspec".into();
        let err = cfg
            .validate(path)
            .expect_err("msgspec should not be an accepted class-default");
        let msg = format!("{err}");
        assert!(msg.contains("msgspec"), "got {msg}");
        assert!(msg.contains("dataclass"), "got {msg}");
    }

    #[test]
    fn skip_decoration_bases_defaults_empty() {
        let cfg = TyphonConfig::default();
        assert!(
            cfg.emit.skip_decoration_bases.is_empty(),
            "expected default skip-decoration-bases to be empty, got {:?}",
            cfg.emit.skip_decoration_bases
        );
    }

    #[test]
    fn skip_decoration_bases_round_trips_through_toml() {
        let toml_src = "\
[project]
name = \"demo\"

[emit]
skip-decoration-bases = [\"BaseModel\", \"Enum\"]
";
        let parsed: TyphonConfig = toml::from_str(toml_src).expect("parse");
        assert_eq!(
            parsed.emit.skip_decoration_bases,
            vec!["BaseModel".to_string(), "Enum".to_string()]
        );
        let reserialized = parsed
            .to_toml_string()
            .expect("serialize TyphonConfig back to toml");
        assert!(
            reserialized.contains("skip-decoration-bases"),
            "expected kebab-cased field in output, got:\n{reserialized}"
        );
        assert!(
            reserialized.contains("BaseModel") && reserialized.contains("Enum"),
            "expected both base-class names in output, got:\n{reserialized}"
        );
    }

    #[test]
    fn parse_python_target_handles_suffixes() {
        assert_eq!(parse_python_target("3.13"), Some((3, 13)));
        assert_eq!(parse_python_target("3.13t"), Some((3, 13)));
        assert_eq!(parse_python_target("3.14rc1"), Some((3, 14)));
        assert_eq!(parse_python_target("3.13.2"), Some((3, 13)));
        assert_eq!(parse_python_target("3.15"), Some((3, 15)));
        assert_eq!(parse_python_target("3.15t"), Some((3, 15)));
        assert_eq!(parse_python_target("3.15.2"), Some((3, 15)));
        assert_eq!(parse_python_target(""), None);
        assert_eq!(parse_python_target("3"), None);
        assert_eq!(parse_python_target("abc"), None);
    }

    // ── model-extra tests ─────────────────────────────────────────────────

    #[test]
    fn model_extra_defaults_to_forbid() {
        let cfg = TyphonConfig::default();
        assert_eq!(
            cfg.emit.model_extra, "forbid",
            "default model-extra must be \"forbid\", got {:?}",
            cfg.emit.model_extra
        );
    }

    #[test]
    fn validate_accepts_all_model_extra_values() {
        let path = Path::new("typhon.toml");
        for v in ALLOWED_MODEL_EXTRAS {
            let mut cfg = cfg_with_target("3.13");
            cfg.emit.model_extra = (*v).into();
            cfg.validate(path)
                .unwrap_or_else(|e| panic!("model-extra {v:?} should be accepted, got {e}"));
        }
    }

    #[test]
    fn validate_rejects_unknown_strictness_severity() {
        let path = Path::new("typhon.toml");
        // A typo / wrong-case severity must be rejected, not silently ignored
        // (which would leave a CI gate the user believes they enabled off).
        for v in &["eror", "WARN", "loud", "fatal", ""] {
            let mut cfg = cfg_with_target("3.13");
            cfg.strictness.exhaustive_match = (*v).into();
            let err = cfg
                .validate(path)
                .expect_err(&format!("severity {v:?} should be rejected"));
            match err {
                ConfigError::InvalidSeverity { key, value, .. } => {
                    assert_eq!(key, "exhaustive-match");
                    assert_eq!(value, *v);
                }
                other => panic!("expected InvalidSeverity for {v:?}, got {other}"),
            }
        }
        // Valid severities pass.
        for v in &["off", "warn", "error"] {
            let mut cfg = cfg_with_target("3.13");
            cfg.strictness.unused_import = (*v).into();
            cfg.validate(path)
                .unwrap_or_else(|e| panic!("severity {v:?} should be accepted, got {e}"));
        }
    }

    #[test]
    fn validate_rejects_unknown_model_extra() {
        let path = Path::new("typhon.toml");
        for v in &["strict", "error", "", "FORBID", "yes"] {
            let mut cfg = cfg_with_target("3.13");
            cfg.emit.model_extra = (*v).into();
            let err = cfg
                .validate(path)
                .expect_err(&format!("model-extra {v:?} should be rejected"));
            match err {
                ConfigError::InvalidModelExtra { value, allowed, .. } => {
                    assert_eq!(value, *v);
                    assert!(
                        allowed.contains("forbid")
                            && allowed.contains("ignore")
                            && allowed.contains("allow"),
                        "expected allowed list to mention all three values, got {allowed}"
                    );
                }
                other => panic!("expected InvalidModelExtra for {v:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn model_extra_round_trips_through_toml() {
        let toml_src = "\
[project]
name = \"demo\"

[emit]
model-extra = \"allow\"
";
        let parsed: TyphonConfig = toml::from_str(toml_src).expect("parse");
        assert_eq!(
            parsed.emit.model_extra, "allow",
            "model-extra should be \"allow\", got {:?}",
            parsed.emit.model_extra
        );
    }

    // ── stub-check tests ──────────────────────────────────────────────────

    // ── unknown-key / checker hardening tests ─────────────────────────────

    #[test]
    fn deny_unknown_top_level_section() {
        // A typo'd section name (`[pyhton]`) must be a hard parse error, not
        // silently dropped — otherwise the intended `[python] target` never
        // takes effect and the user gets a default-3.13 build.
        let err = toml::from_str::<TyphonConfig>(
            "[project]\nname = \"x\"\n\n[pyhton]\ntarget = \"3.14\"\n",
        )
        .expect_err("unknown section should be rejected");
        assert!(
            err.to_string().contains("pyhton") || err.to_string().contains("unknown"),
            "got {err}"
        );
    }

    #[test]
    fn deny_typod_python_target_key() {
        // `taget` instead of `target`: must error rather than silently leaving
        // the default 3.13 in place (finding: typo'd target → wrong build).
        let err = toml::from_str::<TyphonConfig>(
            "[project]\nname = \"x\"\n\n[python]\ntaget = \"3.14\"\n",
        )
        .expect_err("typo'd key should be rejected");
        assert!(
            err.to_string().contains("taget") || err.to_string().contains("unknown"),
            "got {err}"
        );
    }

    #[test]
    fn deny_unknown_strictness_key() {
        let err = toml::from_str::<TyphonConfig>(
            "[project]\nname = \"x\"\n\n[strictness]\nno-implict-any = false\n",
        )
        .expect_err("typo'd strictness key should be rejected");
        assert!(err.to_string().contains("unknown") || err.to_string().contains("implict"));
    }

    #[test]
    fn dependencies_section_still_accepts_arbitrary_packages() {
        // deny_unknown_fields must NOT reject package names inside
        // [dependencies]/[dev-dependencies] — those are map keys, not struct
        // fields. Guards against over-tightening the corpus.
        let cfg: TyphonConfig = toml::from_str(
            "[project]\nname = \"x\"\n\n[dependencies]\nnumpy = \"*\"\nhttpx = \">=0.27\"\n",
        )
        .expect("dependency map keys must parse");
        assert_eq!(cfg.dependencies.get("numpy").map(String::as_str), Some("*"));
    }

    #[test]
    fn validate_accepts_known_checkers() {
        let path = Path::new("typhon.toml");
        for v in ALLOWED_CHECKERS {
            let mut cfg = cfg_with_target("3.13");
            cfg.checker.external = (*v).into();
            cfg.validate(path)
                .unwrap_or_else(|e| panic!("checker {v:?} should be accepted, got {e}"));
        }
    }

    #[test]
    fn validate_rejects_unknown_checker() {
        let path = Path::new("typhon.toml");
        for v in &["mypy", "pyright", "pyre", ""] {
            let mut cfg = cfg_with_target("3.13");
            cfg.checker.external = (*v).into();
            let err = cfg
                .validate(path)
                .expect_err(&format!("checker {v:?} should be rejected"));
            match err {
                ConfigError::InvalidChecker { value, allowed, .. } => {
                    assert_eq!(value, *v);
                    assert!(allowed.contains("ty"), "got {allowed}");
                }
                other => panic!("expected InvalidChecker for {v:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn stub_check_defaults_to_error() {
        let cfg = TyphonConfig::default();
        assert_eq!(
            cfg.strictness.stub_check, "error",
            "default stub-check must be \"error\", got {:?}",
            cfg.strictness.stub_check
        );
    }

    #[test]
    fn stub_check_round_trips_through_toml() {
        let toml_src = "\
[project]
name = \"demo\"

[strictness]
stub-check = \"warn\"
";
        let parsed: TyphonConfig = toml::from_str(toml_src).expect("parse");
        assert_eq!(
            parsed.strictness.stub_check, "warn",
            "stub-check should be \"warn\", got {:?}",
            parsed.strictness.stub_check
        );
    }

    // ── [optimise] level tests ────────────────────────────────────────────

    #[test]
    fn optimise_level_defaults_to_zero() {
        let cfg = TyphonConfig::default();
        assert_eq!(cfg.optimise.level, 0);
        // With no [optimise] section at all, the same default applies.
        let parsed: TyphonConfig =
            toml::from_str("[project]\nname = \"x\"\n").expect("parse without [optimise]");
        assert_eq!(parsed.optimise.level, 0);
    }

    #[test]
    fn optimise_level_parses_zero_and_one() {
        for lvl in [0u8, 1] {
            let src = format!("[project]\nname = \"x\"\n\n[optimise]\nlevel = {lvl}\n");
            let parsed: TyphonConfig =
                toml::from_str(&src).unwrap_or_else(|e| panic!("level {lvl} should parse: {e}"));
            assert_eq!(parsed.optimise.level, lvl);
            parsed
                .validate(Path::new("typhon.toml"))
                .unwrap_or_else(|e| panic!("level {lvl} should validate: {e}"));
        }
    }

    #[test]
    fn optimise_level_two_is_rejected_by_validate() {
        // `2` fits in u8 so it parses; validate() must reject it.
        let parsed: TyphonConfig =
            toml::from_str("[project]\nname = \"x\"\n\n[optimise]\nlevel = 2\n")
                .expect("level = 2 parses (u8) then fails validation");
        let err = parsed
            .validate(Path::new("typhon.toml"))
            .expect_err("level = 2 should be rejected");
        match err {
            ConfigError::InvalidOptimiseLevel { value, .. } => assert_eq!(value, 2),
            other => panic!("expected InvalidOptimiseLevel, got {other:?}"),
        }
        // Message names the offending file and the allowed values.
        let msg = format!("{err}");
        assert!(msg.contains("typhon.toml"), "got {msg}");
        assert!(msg.contains("0, 1"), "got {msg}");
    }

    #[test]
    fn optimise_level_non_integer_is_rejected_at_parse() {
        // A string value can't deserialise into `u8` — a hard parse error.
        let err = toml::from_str::<TyphonConfig>(
            "[project]\nname = \"x\"\n\n[optimise]\nlevel = \"1\"\n",
        )
        .expect_err("string level should be rejected at parse time");
        // Surfaced as a serde/toml parse error, not silently defaulted.
        let _ = err;
    }

    #[test]
    fn optimise_section_rejects_unknown_keys() {
        let err = toml::from_str::<TyphonConfig>(
            "[project]\nname = \"x\"\n\n[optimise]\nlevel = 1\nlvl = 2\n",
        )
        .expect_err("unknown key inside [optimise] should be rejected");
        assert!(
            err.to_string().contains("lvl") || err.to_string().contains("unknown"),
            "got {err}"
        );
    }

    // ── explicit-wins optimise resolution tests ───────────────────────────

    #[test]
    fn level_one_flips_the_four_optimise_defaults() {
        let mut cfg: TyphonConfig =
            toml::from_str("[project]\nname = \"x\"\n\n[optimise]\nlevel = 1\n").expect("parse");
        // Absent from [strictness] → None before resolution.
        assert_eq!(cfg.strictness.auto_memoise, None);
        cfg.resolve_optimise(/* cli_force_level1 = */ false);
        assert_eq!(cfg.strictness.auto_memoise, Some(true));
        assert_eq!(cfg.strictness.auto_gather, Some(true));
        assert_eq!(cfg.strictness.auto_parallel, Some(true));
        assert_eq!(cfg.strictness.pgo_memoise, Some(true));
    }

    #[test]
    fn level_zero_keeps_the_four_optimise_defaults_off() {
        let mut cfg = TyphonConfig::default();
        cfg.resolve_optimise(false);
        assert_eq!(cfg.strictness.auto_memoise, Some(false));
        assert_eq!(cfg.strictness.auto_gather, Some(false));
        assert_eq!(cfg.strictness.auto_parallel, Some(false));
        assert_eq!(cfg.strictness.pgo_memoise, Some(false));
    }

    #[test]
    fn explicit_strictness_false_survives_level_one() {
        // level = 1 but an explicit `auto-parallel = false` must win.
        let mut cfg: TyphonConfig = toml::from_str(
            "[project]\nname = \"x\"\n\n[optimise]\nlevel = 1\n\n[strictness]\nauto-parallel = false\n",
        )
        .expect("parse");
        assert_eq!(cfg.strictness.auto_parallel, Some(false));
        cfg.resolve_optimise(false);
        // The explicit knob stays off; the other three still flip on.
        assert_eq!(cfg.strictness.auto_parallel, Some(false));
        assert_eq!(cfg.strictness.auto_memoise, Some(true));
        assert_eq!(cfg.strictness.auto_gather, Some(true));
        assert_eq!(cfg.strictness.pgo_memoise, Some(true));
    }

    #[test]
    fn cli_optimise_override_forces_level_one() {
        // Config says level 0, but `-O` (cli_force_level1 = true) flips the
        // four defaults on — without overriding an explicit `= false`.
        let mut cfg: TyphonConfig =
            toml::from_str("[project]\nname = \"x\"\n\n[strictness]\nauto-gather = false\n")
                .expect("parse");
        cfg.resolve_optimise(/* cli_force_level1 = */ true);
        assert_eq!(cfg.strictness.auto_memoise, Some(true));
        assert_eq!(cfg.strictness.auto_parallel, Some(true));
        assert_eq!(cfg.strictness.pgo_memoise, Some(true));
        // Explicit false still wins over the CLI flag.
        assert_eq!(cfg.strictness.auto_gather, Some(false));
    }

    #[test]
    fn resolve_optimise_is_idempotent() {
        let mut cfg = TyphonConfig::default();
        cfg.resolve_optimise(true);
        let snapshot = (
            cfg.strictness.auto_memoise,
            cfg.strictness.auto_gather,
            cfg.strictness.auto_parallel,
            cfg.strictness.pgo_memoise,
        );
        cfg.resolve_optimise(false); // second call must not un-flip resolved values
        assert_eq!(
            (
                cfg.strictness.auto_memoise,
                cfg.strictness.auto_gather,
                cfg.strictness.auto_parallel,
                cfg.strictness.pgo_memoise,
            ),
            snapshot
        );
    }

    #[test]
    fn optimise_knobs_absent_omitted_from_toml_round_trip() {
        // A fresh (unresolved) config has None for the four knobs, so
        // `to_toml_string` must omit them — keeping `tyc add`'s rewrite and
        // the `tyc init` scaffold clean.
        let cfg = TyphonConfig::default();
        let out = cfg.to_toml_string().expect("serialise");
        // Match the `key =` assignment form so the check isn't fooled by an
        // unrelated key that merely contains the name as a substring (e.g.
        // `auto-parallel-reductions`, which is a distinct always-serialised knob).
        assert!(
            !out.contains("auto-memoise ="),
            "absent optimise knob should not be serialised, got:\n{out}"
        );
        assert!(!out.contains("auto-gather ="), "got:\n{out}");
        assert!(!out.contains("auto-parallel ="), "got:\n{out}");
        assert!(!out.contains("pgo-memoise ="), "got:\n{out}");
        // It should still re-parse cleanly.
        let _: TyphonConfig = toml::from_str(&out).expect("round-trip parse");
    }

    // ── new strictness knob tests ─────────────────────────────────────────

    #[test]
    fn new_strictness_knobs_have_expected_defaults() {
        let cfg = TyphonConfig::default();
        assert!(cfg.strictness.suggest_perf, "suggest-perf defaults true");
        assert!(
            cfg.strictness.suggest_parallel,
            "suggest-parallel defaults true"
        );
        assert!(
            !cfg.strictness.auto_parallel_reductions,
            "auto-parallel-reductions defaults false"
        );
        assert_eq!(
            cfg.strictness.parallel_backend, "threads",
            "parallel-backend defaults to threads"
        );
    }

    #[test]
    fn new_strictness_knobs_round_trip_through_toml() {
        let toml_src = "\
[project]
name = \"demo\"

[strictness]
suggest-perf = false
suggest-parallel = false
auto-parallel-reductions = true
parallel-backend = \"interpreters\"
";
        let parsed: TyphonConfig = toml::from_str(toml_src).expect("parse");
        assert!(!parsed.strictness.suggest_perf);
        assert!(!parsed.strictness.suggest_parallel);
        assert!(parsed.strictness.auto_parallel_reductions);
        assert_eq!(parsed.strictness.parallel_backend, "interpreters");
        parsed
            .validate(Path::new("typhon.toml"))
            .expect("interpreters backend should validate");
    }

    #[test]
    fn validate_accepts_all_parallel_backends() {
        let path = Path::new("typhon.toml");
        for v in ALLOWED_PARALLEL_BACKENDS {
            let mut cfg = cfg_with_target("3.13");
            cfg.strictness.parallel_backend = (*v).into();
            cfg.validate(path)
                .unwrap_or_else(|e| panic!("parallel-backend {v:?} should be accepted, got {e}"));
        }
    }

    #[test]
    fn validate_rejects_unknown_parallel_backend() {
        let path = Path::new("typhon.toml");
        for v in &["processes", "async", "", "THREADS"] {
            let mut cfg = cfg_with_target("3.13");
            cfg.strictness.parallel_backend = (*v).into();
            let err = cfg
                .validate(path)
                .expect_err(&format!("parallel-backend {v:?} should be rejected"));
            match err {
                ConfigError::InvalidParallelBackend { value, allowed, .. } => {
                    assert_eq!(value, *v);
                    assert!(
                        allowed.contains("threads") && allowed.contains("interpreters"),
                        "got {allowed}"
                    );
                }
                other => panic!("expected InvalidParallelBackend for {v:?}, got {other:?}"),
            }
        }
    }
}
