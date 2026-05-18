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
}

impl Default for EmitConfig {
    fn default() -> Self {
        Self {
            class_default: "dataclass".into(),
            format: true,
        }
    }
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
}

impl Default for StrictnessConfig {
    fn default() -> Self {
        Self {
            no_implicit_any: true,
            unused_import: "error".into(),
            exhaustive_match: "error".into(),
            methods_in_class_body: "warn".into(),
            auto_memoise: false,
            auto_gather: false,
            pgo_memoise: false,
            pgo_min_calls: 100,
            auto_parallel: false,
            parallel_min_size: 64,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EnvConfig {
    /// Env vars that must be resolvable at build time for `comptime env()`.
    pub required: Vec<String>,
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
}

/// Errors that can occur when loading a `typhon.toml` file.
#[derive(Debug)]
pub enum ConfigError {
    Io { path: String, cause: String },
    Parse { path: String, cause: String },
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
        }
    }
}

impl std::error::Error for ConfigError {}
