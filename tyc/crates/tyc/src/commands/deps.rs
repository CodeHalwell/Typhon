//! `tyc add` / `tyc remove` / `tyc sync` — Typhon's lightweight package
//! manager surface over `uv`.
//!
//! Design: `typhon.toml` is the source of truth for direct dependencies.
//! Operations write the dependency back into the `[dependencies]` (or
//! `[dev-dependencies]`) table and shell out to `uv` to drive the actual
//! install / resolution.  `tyc sync` materialises the current
//! `typhon.toml` into a minimal `pyproject.toml` (PEP 621) so `uv sync`
//! can build the lock file and install into the environment.
//!
//! `uv` itself is not bundled — when it isn't on `PATH`, the commands
//! still rewrite `typhon.toml` (so the manifest stays consistent across
//! machines) and surface a clear "install uv" message rather than
//! pretending success.

use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Args;
use miette::{miette, Result};

use crate::config::TyphonConfig;

/// Arguments for `tyc add`.
#[derive(Args, Debug)]
pub struct AddArgs {
    /// Package(s) to add, optionally with a version specifier (`pkg@1.2`,
    /// `pkg>=1`, or bare `pkg` for the latest).
    #[arg(value_name = "PACKAGE", required = true)]
    pub packages: Vec<String>,

    /// Add to `[dev-dependencies]` instead of `[dependencies]`.
    #[arg(long)]
    pub dev: bool,

    /// Skip the `uv` install step. Useful for CI scripts that batch up
    /// edits and call `tyc sync` once at the end.
    #[arg(long)]
    pub no_sync: bool,

    /// Project directory (defaults to current directory).
    #[arg(long, value_name = "DIR", default_value = ".")]
    pub dir: PathBuf,
}

/// Arguments for `tyc remove`.
#[derive(Args, Debug)]
pub struct RemoveArgs {
    /// Package(s) to remove from the manifest.
    #[arg(value_name = "PACKAGE", required = true)]
    pub packages: Vec<String>,

    /// Remove from `[dev-dependencies]` instead of `[dependencies]`.
    #[arg(long)]
    pub dev: bool,

    /// Skip the `uv` uninstall step.
    #[arg(long)]
    pub no_sync: bool,

    /// Project directory (defaults to current directory).
    #[arg(long, value_name = "DIR", default_value = ".")]
    pub dir: PathBuf,
}

/// Arguments for `tyc sync`.
#[derive(Args, Debug)]
pub struct SyncArgs {
    /// Project directory (defaults to current directory).
    #[arg(value_name = "PATH", default_value = ".")]
    pub path: PathBuf,

    /// Print the generated `pyproject.toml` to stdout instead of writing
    /// it to disk and invoking `uv sync`. Useful for previewing what the
    /// command would do.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run_add(args: AddArgs) -> Result<()> {
    let (toml_path, mut config) = load_or_default(&args.dir)?;
    let project_root = toml_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| args.dir.clone());

    let mut new_pkgs: Vec<String> = Vec::new();
    for spec in &args.packages {
        let (name, version) = split_spec(spec);
        let table = if args.dev {
            &mut config.dev_dependencies
        } else {
            &mut config.dependencies
        };
        table.insert(name.to_owned(), version.unwrap_or("*").to_owned());
        new_pkgs.push(spec.clone());
    }

    write_typhon_toml(&toml_path, &config)?;
    println!(
        "updated {} ({} package{})",
        toml_path.display(),
        new_pkgs.len(),
        if new_pkgs.len() == 1 { "" } else { "s" }
    );

    if args.no_sync {
        return Ok(());
    }
    materialise_pyproject(&project_root, &config)?;
    run_uv_sync(&project_root)
}

pub fn run_remove(args: RemoveArgs) -> Result<()> {
    let (toml_path, mut config) = load_or_default(&args.dir)?;
    let project_root = toml_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| args.dir.clone());

    let mut removed = 0usize;
    let table = if args.dev {
        &mut config.dev_dependencies
    } else {
        &mut config.dependencies
    };
    for name in &args.packages {
        if table.remove(name).is_some() {
            removed += 1;
        } else {
            eprintln!("warning: {name} was not listed in typhon.toml");
        }
    }
    if removed == 0 {
        return Ok(());
    }

    write_typhon_toml(&toml_path, &config)?;
    println!("updated {} ({removed} removed)", toml_path.display());

    if args.no_sync {
        return Ok(());
    }
    materialise_pyproject(&project_root, &config)?;
    run_uv_sync(&project_root)
}

pub fn run_sync(args: SyncArgs) -> Result<()> {
    let (toml_path, config) = load_or_default(&args.path)?;
    let project_root = toml_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| args.path.clone());

    let pyproject = render_pyproject(&config);
    if args.dry_run {
        println!("{pyproject}");
        return Ok(());
    }
    std::fs::write(project_root.join("pyproject.toml"), &pyproject)
        .map_err(|e| miette!("cannot write pyproject.toml: {e}"))?;
    println!("wrote {}", project_root.join("pyproject.toml").display());
    run_uv_sync(&project_root)
}

// ── helpers ────────────────────────────────────────────────────────────────

/// Locate and load `typhon.toml`. Returns the absolute path of the file
/// plus the parsed config; falls back to a default config rooted at
/// `dir/typhon.toml` when no manifest exists yet (so `tyc add` from a
/// brand-new directory still works).
fn load_or_default(dir: &Path) -> Result<(PathBuf, TyphonConfig)> {
    let canon = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    match TyphonConfig::load(&canon) {
        Ok(Some(found)) => Ok(found),
        Ok(None) => Ok((canon.join("typhon.toml"), TyphonConfig::default())),
        Err(e) => Err(miette!("{e}")),
    }
}

fn write_typhon_toml(path: &Path, config: &TyphonConfig) -> Result<()> {
    let text = config
        .to_toml_string()
        .map_err(|e| miette!("cannot serialise typhon.toml: {e}"))?;
    std::fs::write(path, text).map_err(|e| miette!("cannot write {}: {e}", path.display()))
}

/// Split a CLI dep spec like `requests`, `requests@2.31`, or
/// `requests>=2,<3` into `(name, optional-version-spec)`.
///
/// Pure function so it's exhaustively unit-tested below.
pub(crate) fn split_spec(spec: &str) -> (&str, Option<&str>) {
    if let Some((n, v)) = spec.split_once("@") {
        return (n.trim(), Some(v.trim()));
    }
    for op in ["===", "==", ">=", "<=", "!=", "~=", ">", "<"] {
        if let Some(idx) = spec.find(op) {
            let (n, v) = spec.split_at(idx);
            return (n.trim(), Some(v.trim()));
        }
    }
    (spec.trim(), None)
}

/// Render the `[dependencies]` / `[dev-dependencies]` tables as a PEP 621
/// `pyproject.toml`. Other tables (`[project]`, `[python]`) are mapped
/// onto their PEP 621 equivalents.
pub(crate) fn render_pyproject(config: &TyphonConfig) -> String {
    let mut out = String::new();
    out.push_str("# generated by `tyc sync` — edits will be overwritten\n");
    out.push_str("[project]\n");
    out.push_str(&format!(
        "name = \"{}\"\n",
        toml_escape(default_str(&config.project.name, "untitled"))
    ));
    out.push_str(&format!(
        "version = \"{}\"\n",
        toml_escape(default_str(&config.project.version, "0.1.0"))
    ));
    let py = default_str(&config.python.target, "3.13");
    out.push_str(&format!(
        "requires-python = \">={py}\"\n",
        py = toml_escape(py)
    ));
    let mut deps: Vec<String> = config
        .dependencies
        .iter()
        .map(|(k, v)| pep508(k, v))
        .collect();
    deps.sort();
    if !deps.is_empty() {
        out.push_str("dependencies = [\n");
        for d in &deps {
            out.push_str(&format!("    \"{}\",\n", toml_escape(d)));
        }
        out.push_str("]\n");
    } else {
        out.push_str("dependencies = []\n");
    }
    if !config.dev_dependencies.is_empty() {
        out.push_str("\n[project.optional-dependencies]\n");
        let mut dev: Vec<String> = config
            .dev_dependencies
            .iter()
            .map(|(k, v)| pep508(k, v))
            .collect();
        dev.sort();
        out.push_str("dev = [\n");
        for d in &dev {
            out.push_str(&format!("    \"{}\",\n", toml_escape(d)));
        }
        out.push_str("]\n");
    }
    out
}

/// Build a PEP 508 dependency line from a name + manifest version string.
///
/// The Typhon manifest accepts a few shapes for convenience:
///   * `"*"` / `""` — any version (emits a bare name).
///   * a comparator already in the string (`>=2.0`, `==1.5`) — passes
///     through unchanged.
///   * a bare version (`2.31`) — implicitly anchored with `==`.
fn pep508(name: &str, version: &str) -> String {
    let v = version.trim();
    if v.is_empty() || v == "*" {
        return name.to_owned();
    }
    let starts_with_op = ["===", "==", ">=", "<=", "!=", "~=", ">", "<", "@"]
        .iter()
        .any(|op| v.starts_with(op));
    if starts_with_op {
        return format!("{name}{v}");
    }
    format!("{name}=={v}")
}

fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn default_str<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() {
        fallback
    } else {
        value
    }
}

fn materialise_pyproject(project_root: &Path, config: &TyphonConfig) -> Result<()> {
    let path = project_root.join("pyproject.toml");
    let text = render_pyproject(config);
    std::fs::write(&path, text).map_err(|e| miette!("cannot write {}: {e}", path.display()))
}

fn run_uv_sync(project_root: &Path) -> Result<()> {
    if !has_uv() {
        eprintln!(
            "warning: `uv` not found on PATH — typhon.toml updated but no install was run. \
             Install uv (https://docs.astral.sh/uv/) and re-run `tyc sync`."
        );
        return Ok(());
    }
    let status = Command::new("uv")
        .arg("sync")
        .current_dir(project_root)
        .status()
        .map_err(|e| miette!("cannot spawn `uv`: {e}"))?;
    if !status.success() {
        return Err(miette!("`uv sync` failed with status {status}"));
    }
    Ok(())
}

fn has_uv() -> bool {
    Command::new("uv")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_spec_handles_bare_name() {
        assert_eq!(split_spec("requests"), ("requests", None));
    }

    #[test]
    fn split_spec_handles_at_form() {
        assert_eq!(split_spec("requests@2.31"), ("requests", Some("2.31")));
    }

    #[test]
    fn split_spec_handles_pep440_op() {
        assert_eq!(split_spec("requests>=2,<3"), ("requests", Some(">=2,<3")));
        assert_eq!(split_spec("urllib3==1.26.0"), ("urllib3", Some("==1.26.0")));
    }

    #[test]
    fn pep508_renders_bare_name_for_star() {
        assert_eq!(pep508("requests", "*"), "requests");
        assert_eq!(pep508("requests", ""), "requests");
    }

    #[test]
    fn pep508_anchors_bare_version_with_eq() {
        assert_eq!(pep508("requests", "2.31"), "requests==2.31");
    }

    #[test]
    fn pep508_passes_through_existing_comparator() {
        assert_eq!(pep508("requests", ">=2.0"), "requests>=2.0");
        assert_eq!(pep508("requests", "==1.5"), "requests==1.5");
    }

    #[test]
    fn render_pyproject_includes_runtime_and_dev_tables() {
        let mut cfg = TyphonConfig::default();
        cfg.project.name = "demo".into();
        cfg.dependencies.insert("requests".into(), "*".into());
        cfg.dependencies.insert("rich".into(), ">=13".into());
        cfg.dev_dependencies.insert("pytest".into(), "8.2".into());
        let out = render_pyproject(&cfg);
        assert!(out.contains("name = \"demo\""), "{out}");
        assert!(out.contains("requests"), "{out}");
        assert!(out.contains("rich>=13"), "{out}");
        assert!(out.contains("[project.optional-dependencies]"), "{out}");
        assert!(out.contains("pytest==8.2"), "{out}");
    }

    #[test]
    fn render_pyproject_handles_empty_dependencies() {
        let cfg = TyphonConfig::default();
        let out = render_pyproject(&cfg);
        assert!(out.contains("dependencies = []"));
        assert!(!out.contains("[project.optional-dependencies]"));
    }
}
