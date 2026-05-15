//! `typhon.toml` configuration file parsing.

use serde::{Deserialize, Serialize};
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
#[serde(default)]
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
#[serde(default)]
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
#[serde(default)]
pub struct StrictnessConfig {
    pub no_implicit_any: bool,
    pub unused_import: String,
    pub exhaustive_match: String,
}

impl Default for StrictnessConfig {
    fn default() -> Self {
        Self {
            no_implicit_any: true,
            unused_import: "error".into(),
            exhaustive_match: "error".into(),
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
    pub fn load(start: &Path) -> Option<(PathBuf, Self)> {
        let mut dir = start.to_path_buf();
        loop {
            let candidate = dir.join("typhon.toml");
            if candidate.exists() {
                let text = std::fs::read_to_string(&candidate).ok()?;
                let config: Self = toml::from_str(&text).ok()?;
                return Some((candidate, config));
            }
            if !dir.pop() {
                break;
            }
        }
        None
    }

    /// Serialize this config back to TOML.
    pub fn to_toml_string(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }
}
