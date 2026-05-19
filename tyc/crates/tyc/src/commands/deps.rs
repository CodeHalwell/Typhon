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
    // This header lands when there is no existing pyproject.toml to
    // merge into. Subsequent `tyc build` / `tyc sync` runs use the
    // merge-aware path (`apply_owned_keys`), which preserves any
    // `[tool.*]` tables, comments, and `[project]` keys the user
    // adds — only the keys this tool owns are rewritten.
    out.push_str(
        "# Generated by tyc. The following keys are owned by tyc and \
         will be rewritten\n\
         # on each `tyc build` / `tyc sync`:\n\
         #   project.{name, version, requires-python, dependencies}\n\
         #   dependency-groups.dev\n\
         # Everything else (including `[tool.*]`) is preserved across rewrites.\n",
    );
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
        // PEP 735 dependency-groups: `uv sync` installs the `dev` group
        // by default (no `--extra` flag needed).  An earlier draft used
        // `[project.optional-dependencies]`, but uv only installs those
        // when explicitly enabled via `--extra dev` / `--all-extras`,
        // so `tyc add --dev pytest` was silently a no-op.
        out.push_str("\n[dependency-groups]\n");
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

/// Bootstrap the Python environment for a `tyc build`.
///
/// Steps, in order:
///   1. Merge our owned keys into `pyproject.toml` at `project_root`,
///      preserving anything we don't own (`[tool.*]`, `[build-system]`,
///      free-text comments, user-managed `[project]` keys like
///      `authors` / `readme`). If `pyproject.toml` doesn't exist yet,
///      a minimal file is written using [`render_pyproject`].
///   2. Run `uv sync` to materialise `.venv` and install the manifest.
///      Failure is downgraded to a warning so the build can still emit
///      `.py` artefacts (the codegen output is useful regardless of
///      whether the venv resolved).
///
/// The keys we own — and therefore overwrite — are:
///   * `project.name`
///   * `project.version`
///   * `project.requires-python`
///   * `project.dependencies`
///   * `dependency-groups.dev` (only if `[dev-dependencies]` is non-empty)
///
/// Every other key is left untouched, including any tables added by
/// the user under `[project]` itself.
pub fn bootstrap_python_env(project_root: &Path, config: &TyphonConfig) -> Result<()> {
    merge_pyproject(project_root, config)?;
    run_uv_sync_warning(project_root);
    Ok(())
}

/// Read-modify-write `pyproject.toml` with [`toml_edit::DocumentMut`] so
/// formatting, comments, and unrelated tables are preserved. When no
/// existing file is present we fall back to a minimal greenfield render
/// via [`render_pyproject`].
fn merge_pyproject(project_root: &Path, config: &TyphonConfig) -> Result<()> {
    let path = project_root.join("pyproject.toml");
    if !path.exists() {
        let text = render_pyproject(config);
        std::fs::write(&path, text).map_err(|e| miette!("cannot write {}: {e}", path.display()))?;
        return Ok(());
    }

    let existing = std::fs::read_to_string(&path)
        .map_err(|e| miette!("cannot read {}: {e}", path.display()))?;
    let mut doc: toml_edit::DocumentMut = existing
        .parse()
        .map_err(|e| miette!("cannot parse {} as TOML: {e}", path.display()))?;

    apply_owned_keys(&mut doc, config);

    std::fs::write(&path, doc.to_string())
        .map_err(|e| miette!("cannot write {}: {e}", path.display()))
}

/// Overwrite the keys this tool owns inside the document, leaving every
/// other key untouched. Pulled out so it can be unit-tested without
/// touching the filesystem.
pub(crate) fn apply_owned_keys(doc: &mut toml_edit::DocumentMut, config: &TyphonConfig) {
    use toml_edit::{value, Array, Item, Table};

    // ── [project] ────────────────────────────────────────────────────────────
    // A malformed user pyproject.toml could declare `project` as a string
    // or array. We don't want a panic mid-build for what is ultimately a
    // user-config issue; warn and skip the merge so the codegen output
    // still lands.
    let project_entry = doc
        .entry("project")
        .or_insert_with(|| Item::Table(Table::new()));
    let Some(project) = project_entry.as_table_mut() else {
        eprintln!(
            "warning: [project] in pyproject.toml is not a table — \
             skipping pyproject merge. Build artefacts still emitted."
        );
        return;
    };

    project.insert("name", value(default_str(&config.project.name, "untitled")));
    project.insert(
        "version",
        value(default_str(&config.project.version, "0.1.0")),
    );
    let py = default_str(&config.python.target, "3.13");
    project.insert("requires-python", value(format!(">={py}")));

    let mut deps_arr = Array::new();
    let mut deps: Vec<String> = config
        .dependencies
        .iter()
        .map(|(k, v)| pep508(k, v))
        .collect();
    deps.sort();
    for d in deps {
        deps_arr.push(d);
    }
    project.insert("dependencies", value(deps_arr));

    // ── [dependency-groups] ──────────────────────────────────────────────────
    // Only managed when [dev-dependencies] is non-empty; if the user
    // removes every dev dep, we clear our `dev` array but leave the
    // rest of the group table (and any other groups) alone. As with
    // [project] above, we warn-and-skip rather than panic when the
    // user has shadowed the section with a non-table value.
    if !config.dev_dependencies.is_empty() {
        let groups_entry = doc
            .entry("dependency-groups")
            .or_insert_with(|| Item::Table(Table::new()));
        let Some(groups) = groups_entry.as_table_mut() else {
            eprintln!(
                "warning: [dependency-groups] in pyproject.toml is not a \
                 table — skipping dev group merge."
            );
            return;
        };
        let mut dev_arr = Array::new();
        let mut dev: Vec<String> = config
            .dev_dependencies
            .iter()
            .map(|(k, v)| pep508(k, v))
            .collect();
        dev.sort();
        for d in dev {
            dev_arr.push(d);
        }
        groups.insert("dev", value(dev_arr));
    } else if let Some(groups) = doc
        .get_mut("dependency-groups")
        .and_then(|g| g.as_table_mut())
    {
        groups.remove("dev");
    }
}

/// `run_uv_sync` variant that downgrades failure to a warning. Used by
/// `tyc build`, where the codegen artefacts are still worth emitting
/// even when the install step trips over a transient issue (no network,
/// uv resolver error, etc.). The explicit-intent commands (`tyc sync` /
/// `tyc add` / `tyc remove`) keep their hard-error behaviour.
fn run_uv_sync_warning(project_root: &Path) {
    if !has_uv() {
        eprintln!(
            "warning: `uv` not found on PATH — pyproject.toml was updated \
             but no `.venv` install was run. Install uv \
             (https://docs.astral.sh/uv/) to bootstrap the environment."
        );
        return;
    }
    let status = Command::new("uv")
        .arg("sync")
        .current_dir(project_root)
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("warning: `uv sync` exited with {s} — build artefacts were still emitted")
        }
        Err(e) => eprintln!("warning: cannot spawn `uv`: {e}"),
    }
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
        // Dev deps go in `[dependency-groups]` so `uv sync` installs them
        // by default (PEP 735); the old `optional-dependencies` table
        // required `--extra dev` and silently skipped them otherwise.
        assert!(out.contains("[dependency-groups]"), "{out}");
        assert!(out.contains("pytest==8.2"), "{out}");
    }

    #[test]
    fn render_pyproject_handles_empty_dependencies() {
        let cfg = TyphonConfig::default();
        let out = render_pyproject(&cfg);
        assert!(out.contains("dependencies = []"));
        assert!(!out.contains("[dependency-groups]"));
    }

    // ── merge-aware bootstrap ──────────────────────────────────────────────

    #[test]
    fn apply_owned_keys_preserves_user_tables_and_keys() {
        // The user's pyproject.toml has [tool.ruff], a header comment,
        // and additional [project] keys we don't manage (`authors`,
        // `readme`). The bootstrap must overwrite only the keys we own
        // (`name`, `version`, `requires-python`, `dependencies`) and
        // leave everything else byte-for-byte intact.
        let existing = "\
# user comment at the top
[project]
name = \"old-name\"
version = \"9.9.9\"
authors = [{ name = \"H\", email = \"h@x\" }]
readme = \"README.md\"

[tool.ruff]
line-length = 100
target-version = \"py313\"
";
        let mut doc: toml_edit::DocumentMut = existing.parse().unwrap();
        let mut cfg = TyphonConfig::default();
        cfg.project.name = "verify-model".into();
        cfg.project.version = "0.1.0".into();
        cfg.python.target = "3.13".into();
        apply_owned_keys(&mut doc, &cfg);
        let out = doc.to_string();
        assert!(
            out.starts_with("# user comment at the top\n"),
            "header comment must be preserved; got:\n{out}",
        );
        assert!(
            out.contains("name = \"verify-model\""),
            "our `name` should overwrite the user's; got:\n{out}",
        );
        assert!(
            out.contains("version = \"0.1.0\""),
            "our `version` should overwrite the user's; got:\n{out}",
        );
        assert!(
            !out.contains("old-name") && !out.contains("9.9.9"),
            "old owned values must be gone; got:\n{out}",
        );
        assert!(
            out.contains("requires-python = \">=3.13\""),
            "`requires-python` should be inserted; got:\n{out}",
        );
        assert!(out.contains("dependencies = []"), "{out}");
        assert!(
            out.contains("authors") && out.contains("h@x"),
            "user-managed `authors` must survive; got:\n{out}",
        );
        assert!(
            out.contains("readme = \"README.md\""),
            "user-managed `readme` must survive; got:\n{out}",
        );
        assert!(
            out.contains("[tool.ruff]") && out.contains("line-length = 100"),
            "[tool.ruff] block must be left untouched; got:\n{out}",
        );
    }

    #[test]
    fn apply_owned_keys_writes_dependencies_array_sorted() {
        // Deterministic ordering matters: a build that toggles dep
        // order on every run produces noisy diffs and breaks any
        // downstream tooling that hashes pyproject.toml.
        let mut doc: toml_edit::DocumentMut = "".parse().unwrap();
        let mut cfg = TyphonConfig::default();
        cfg.project.name = "demo".into();
        cfg.dependencies.insert("rich".into(), ">=13".into());
        cfg.dependencies.insert("requests".into(), "*".into());
        apply_owned_keys(&mut doc, &cfg);
        let out = doc.to_string();
        let requests_pos = out.find("requests").expect("requests in output");
        let rich_pos = out.find("rich").expect("rich in output");
        assert!(
            requests_pos < rich_pos,
            "dependencies should be sorted alphabetically; got:\n{out}",
        );
    }

    #[test]
    fn apply_owned_keys_updates_dependency_groups_dev() {
        // Adding a dev dep should populate [dependency-groups].dev
        // even when the user already has unrelated keys in the group
        // table (e.g. a `lint` group they manage themselves).
        let existing = "\
[dependency-groups]
lint = [\"ruff\"]
";
        let mut doc: toml_edit::DocumentMut = existing.parse().unwrap();
        let mut cfg = TyphonConfig::default();
        cfg.project.name = "demo".into();
        cfg.dev_dependencies.insert("pytest".into(), "8.2".into());
        apply_owned_keys(&mut doc, &cfg);
        let out = doc.to_string();
        assert!(
            out.contains("dev") && out.contains("pytest==8.2"),
            "dev group should be written; got:\n{out}",
        );
        assert!(
            out.contains("lint") && out.contains("ruff"),
            "user's `lint` group must survive; got:\n{out}",
        );
    }

    #[test]
    fn apply_owned_keys_clears_dev_when_dev_dependencies_emptied() {
        // If the user removes every dev dep from typhon.toml, the
        // generated `dev` array should go away — but the
        // [dependency-groups] table itself stays so other groups
        // (e.g. a `lint` group the user manages) are not deleted.
        let existing = "\
[dependency-groups]
dev = [\"pytest==1.0\"]
lint = [\"ruff\"]
";
        let mut doc: toml_edit::DocumentMut = existing.parse().unwrap();
        let mut cfg = TyphonConfig::default();
        cfg.project.name = "demo".into();
        apply_owned_keys(&mut doc, &cfg);
        let out = doc.to_string();
        assert!(
            !out.contains("pytest==1.0"),
            "old dev entries must be cleared; got:\n{out}",
        );
        assert!(
            out.contains("lint") && out.contains("ruff"),
            "other groups must survive; got:\n{out}",
        );
    }
}
