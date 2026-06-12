//! `typhon.toml` configuration file parsing.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The contents of a `typhon.toml` project file.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TyphonConfig {
    pub project: ProjectConfig,
    pub python: PythonConfig,
    pub emit: EmitConfig,
    pub strictness: StrictnessConfig,
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
#[serde(default)]
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
#[serde(default, rename_all = "kebab-case")]
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
#[serde(default, rename_all = "kebab-case")]
pub struct EmitConfig {
    /// Default class emission target: `"dataclass"` (default) or `"pydantic"`.
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
pub const ALLOWED_CLASS_DEFAULTS: &[&str] = &["dataclass", "pydantic"];

/// Accepted values for `[emit] model-extra`. Anything else is rejected by
/// [`TyphonConfig::validate`].
pub const ALLOWED_MODEL_EXTRAS: &[&str] = &["forbid", "ignore", "allow"];

fn default_model_extra() -> String {
    "forbid".into()
}

fn default_stub_check() -> String {
    "error".into()
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
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
    /// When true, every function that passes the six-condition purity check
    /// is treated as if the user had written `@memo` — the desugarer emits a
    /// `@functools.cache` decorator. Off by default; opt in per-project.
    pub auto_memoise: bool,
    /// When true, runs of two-or-more consecutive independent `await` calls
    /// inside an `async def` are rewritten into an `asyncio.TaskGroup` block
    /// so they execute concurrently. Independence is determined by static
    /// data-flow on bound names; runs are only folded when every callee is
    /// an `async def` declared in the same module. Off by default — flip on
    /// per-project to apply the rewrite globally. (Phase 4 auto-gather
    /// inference; explicit `gather:` blocks are unaffected.)
    pub auto_gather: bool,
    /// When true (the default), `tyc build` surfaces a `tyc::gather_opportunity`
    /// advice for every run of 2+ adjacent independent awaited calls inside an
    /// `async def` — including awaited method calls on imported clients, which
    /// `auto-gather` never touches. Advice-only: it suggests wrapping the run
    /// in an explicit `gather:` block but never rewrites (concurrency is a
    /// behaviour change the author opts into). Set `false` to silence the
    /// nudge; it never blocks a build regardless.
    pub suggest_gather: bool,
    /// When true, `tyc build` consults `typhon-profile.json` (produced by a
    /// prior `tyc profile` run) and promotes every pure function whose call
    /// count meets [`StrictnessConfig::pgo_min_calls`] to `@functools.cache`,
    /// even if the user did not write `@memo`. Off by default. (Phase 4
    /// profile-guided optimisation; complements `auto-memoise` which acts
    /// on every pure function regardless of profile data.)
    pub pgo_memoise: bool,
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
    pub auto_parallel: bool,
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
}

impl Default for StrictnessConfig {
    fn default() -> Self {
        Self {
            no_implicit_any: true,
            unused_import: "warn".into(),
            exhaustive_match: "error".into(),
            methods_in_class_body: "warn".into(),
            auto_memoise: false,
            auto_gather: false,
            suggest_gather: true,
            pgo_memoise: false,
            pgo_min_calls: 100,
            auto_parallel: false,
            parallel_min_size: 64,
            require_with: "warn".into(),
            blocking_in_async: "warn".into(),
            allow_secret_comptime: false,
            stub_check: "error".into(),
            unintrospectable_dependency: "warn".into(),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
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
#[serde(default, rename_all = "kebab-case")]
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
                        path: candidate.display().to_string(),
                        cause: e.to_string(),
                    }
                })?;
                let config: Self =
                    toml::from_str(&text).map_err(|e| crate::config::ConfigError::Parse {
                        path: candidate.display().to_string(),
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
    /// Accepts the bare-major form (`"3.13"`, `"3.14"`) and the
    /// free-threaded suffix (`"3.13t"`, `"3.14t"`). Anything that
    /// parses below 3.13 — including the still-supported-by-CPython
    /// 3.11 and 3.12 — is a hard error pointing at the offending
    /// `typhon.toml`.
    pub fn validate(&self, source_path: &Path) -> Result<(), ConfigError> {
        let raw = self.python.target.trim();
        let (major, minor) =
            parse_python_target(raw).ok_or_else(|| ConfigError::UnsupportedPythonTarget {
                path: source_path.display().to_string(),
                target: raw.to_owned(),
                reason: "expected a `MAJOR.MINOR` string such as `3.13` or `3.14t`".to_owned(),
            })?;
        if (major, minor) < (3, 13) {
            return Err(ConfigError::UnsupportedPythonTarget {
                path: source_path.display().to_string(),
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
        if !ALLOWED_CLASS_DEFAULTS.contains(&cd) {
            return Err(ConfigError::InvalidClassDefault {
                path: source_path.display().to_string(),
                value: self.emit.class_default.clone(),
                allowed: ALLOWED_CLASS_DEFAULTS.join(", "),
            });
        }
        // Reject `[emit] model-extra = "..."` outside the allow-list.
        let me = self.emit.model_extra.trim();
        if !ALLOWED_MODEL_EXTRAS.contains(&me) {
            return Err(ConfigError::InvalidModelExtra {
                path: source_path.display().to_string(),
                value: self.emit.model_extra.clone(),
                allowed: ALLOWED_MODEL_EXTRAS.join(", "),
            });
        }
        Ok(())
    }
}

/// Parse a `[python] target` value into `(major, minor)`. Accepts the
/// `"3.13"` bare form, the `"3.13t"` free-threaded suffix, and tolerates
/// patch-level strings like `"3.13.2"` by ignoring everything past
/// the second segment. Returns `None` for malformed input.
fn parse_python_target(s: &str) -> Option<(u32, u32)> {
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
        for t in &["3.13", "3.13t", "3.14", "3.14t", "3.13.2"] {
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
    fn validate_accepts_dataclass_and_pydantic() {
        let path = Path::new("typhon.toml");
        for v in &["dataclass", "pydantic"] {
            let mut cfg = cfg_with_target("3.13");
            cfg.emit.class_default = (*v).into();
            cfg.validate(path)
                .unwrap_or_else(|e| panic!("class-default {v} should be accepted, got {e}"));
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
                        allowed.contains("dataclass") && allowed.contains("pydantic"),
                        "expected allowed list to mention dataclass+pydantic, got {allowed}"
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
        assert!(msg.contains("pydantic"), "got {msg}");
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
}
