//! `tyc stubtest` — runtime probe that complements `tyc check --stubs`.
//!
//! `tyc check --stubs` performs an AST-level diff between every `.dty`
//! stub and its sibling `.ty` / `.py` implementation. That catches the
//! common drift sources: a function added on one side and not the other,
//! a parameter rename, a missing class member. What the AST diff *cannot*
//! see are dynamically-created attributes: a base-class `__init_subclass__`
//! that injects fields, a metaclass that registers methods, a `setattr`
//! in a class body, a Pydantic model's auto-generated fields. mypy's
//! `stubtest` proper handles those by *importing* the module at runtime
//! and walking its symbol table via Python introspection.
//!
//! This command wires that runtime probe into the Typhon toolchain:
//!
//! 1. Build the project so every `.dty` becomes a `.pyi` next to a
//!    matching implementation `.py` in the build output directory.
//! 2. Walk the build output for every `.pyi` and derive its Python module
//!    path from the file location relative to the output root.
//! 3. Invoke `python -m mypy.stubtest <module>` as a subprocess with
//!    `PYTHONPATH` pointing at the build output so the import succeeds
//!    against the just-emitted code rather than any installed copy.
//! 4. Report stubtest's findings to stderr verbatim and exit non-zero
//!    when any of them fire.
//!
//! `mypy` is not vendored — users install it themselves
//! (`pip install mypy` or `uv tool install mypy`). The command surfaces a
//! clear "mypy not found" message when the subprocess fails to start.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Args;
use miette::{miette, Result};

use crate::commands::build::{self, BuildArgs};

/// Arguments for `tyc stubtest`.
#[derive(Args, Debug)]
pub struct StubtestArgs {
    /// Project directory (defaults to the current directory).
    #[arg(value_name = "PATH", default_value = ".")]
    pub path: PathBuf,

    /// Build into this directory before running stubtest. If omitted, a
    /// temporary directory is used and removed when the command exits.
    /// Useful in CI to keep the emitted output for later inspection:
    /// `tyc stubtest --out build/`.
    #[arg(long, short = 'o', value_name = "DIR")]
    pub out: Option<PathBuf>,

    /// Skip the build step and run stubtest against an existing output
    /// directory. Requires `--out` so the directory is unambiguous.
    #[arg(long)]
    pub no_build: bool,

    /// Python interpreter to invoke. Defaults to `python3`. Override when
    /// stubtest must run against a specific virtualenv (`--python
    /// .venv/bin/python`) or a non-standard binary name.
    #[arg(long, value_name = "BIN", default_value = "python3")]
    pub python: String,

    /// Continue running stubtest against the remaining modules even when
    /// one of them reports a finding. Without this flag, the first
    /// non-zero stubtest exit terminates the run with an error.
    ///
    /// Useful in CI when you want the complete drift report rather than
    /// just the first failure.
    #[arg(long)]
    pub keep_going: bool,

    /// Extra arguments forwarded to `mypy.stubtest` verbatim
    /// (e.g. `--allowlist`, `--ignore-positional-only`).
    #[arg(last = true)]
    pub stubtest_args: Vec<String>,
}

pub fn run(args: StubtestArgs) -> Result<()> {
    if args.no_build && args.out.is_none() {
        return Err(miette!(
            "--no-build requires --out so the output directory is known"
        ));
    }

    // Canonicalise the project path up front so every downstream
    // computation (build out_dir, subprocess cwd, PYTHONPATH) sees the
    // same absolute root. Without this, a relative `--out` would be
    // joined against `args.path` here AND re-anchored by `tyc build`,
    // producing a duplicated path like
    // `<root>/<project_path>/<project_path>/build`. Resolving once
    // matches the behaviour of `tyc build`'s own resolution.
    let project_root = args
        .path
        .canonicalize()
        .map_err(|e| miette!("cannot resolve project path '{}': {e}", args.path.display()))?;

    let (out_dir, _tempdir_guard) = match (&args.out, args.no_build) {
        (Some(dir), _) => {
            let resolved = if dir.is_absolute() {
                dir.clone()
            } else {
                project_root.join(dir)
            };
            (resolved, None)
        }
        (None, false) => {
            let td = tempfile::tempdir()
                .map_err(|e| miette!("failed to create temp output directory: {e}"))?;
            let path = td.path().to_path_buf();
            (path, Some(td))
        }
        (None, true) => unreachable!("guarded above"),
    };

    if !args.no_build {
        // Pass the already-absolute `out_dir` into `tyc build` so its own
        // resolution stays a no-op (an absolute --out short-circuits the
        // project_root.join in build.rs).
        build::run(BuildArgs {
            path: project_root.clone(),
            out: Some(out_dir.clone()),
            no_format: false,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })?;
    }

    let stubs = collect_pyi_modules(&out_dir)?;
    if stubs.is_empty() {
        println!(
            "tyc stubtest: no `.pyi` stubs found under '{}' — nothing to probe",
            out_dir.display()
        );
        return Ok(());
    }

    println!(
        "tyc stubtest: probing {} module(s) via `{} -m mypy.stubtest`",
        stubs.len(),
        args.python
    );

    let mut failures: Vec<String> = Vec::new();
    for module in &stubs {
        match run_stubtest_for_module(&args, &project_root, &out_dir, module) {
            Ok(()) => {}
            Err(e) => {
                if args.keep_going {
                    eprintln!("{:?}", e);
                    failures.push(module.clone());
                } else {
                    return Err(e);
                }
            }
        }
    }

    if !failures.is_empty() {
        return Err(miette!(
            "stubtest reported drift in {} module(s): {}",
            failures.len(),
            failures.join(", ")
        ));
    }

    println!("tyc stubtest: all {} module(s) pass", stubs.len());
    Ok(())
}

/// Invoke `python -m mypy.stubtest <module>` with `PYTHONPATH` pointing at
/// the build output directory and the working directory anchored at the
/// project root so mypy picks up `mypy.ini` / `pyproject.toml` from the
/// project rather than the caller's cwd and any relative `--allowlist`
/// path forwarded via `stubtest_args` resolves against the project too.
///
/// Captures stdout/stderr so we can distinguish "stubtest found drift"
/// from "stubtest itself failed to start" — the most common case being a
/// missing `mypy` install, which surfaces as `No module named mypy` on
/// stderr. Either way the captured output is streamed to the parent
/// stdio so the user sees stubtest's diagnostics verbatim.
fn run_stubtest_for_module(
    args: &StubtestArgs,
    project_root: &Path,
    out_dir: &Path,
    module: &str,
) -> Result<()> {
    let mut cmd = Command::new(&args.python);
    cmd.arg("-m").arg("mypy.stubtest").arg(module);
    for extra in &args.stubtest_args {
        cmd.arg(extra);
    }
    // Anchor the subprocess at the project root. Mirrors `tyc ty`'s
    // behaviour so mypy/stubtest discover the project's config and any
    // relative paths forwarded via `--` resolve where the user expects.
    cmd.current_dir(project_root);
    // Prepend the build directory to PYTHONPATH so the just-emitted
    // module shadows any installed copy with the same name. Appending
    // instead would let an installed package mask the local build.
    // `out_dir` is absolute by construction (see `run`), so the cwd
    // change above doesn't affect resolution.
    let existing = std::env::var("PYTHONPATH").unwrap_or_default();
    let joined = if existing.is_empty() {
        out_dir.to_string_lossy().into_owned()
    } else {
        format!("{}{}{}", out_dir.display(), path_separator(), existing)
    };
    cmd.env("PYTHONPATH", joined);

    let output = cmd.output().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => miette!(
            "`{}` not found on PATH — install Python (or pass --python to point at \
             your interpreter)",
            args.python,
        ),
        _ => miette!(
            "failed to run `{} -m mypy.stubtest {module}`: {e}",
            args.python
        ),
    })?;

    // Echo the subprocess output to the user's terminal verbatim so
    // stubtest's diagnostics are preserved even though we captured them.
    let _ = std::io::stdout().write_all(&output.stdout);
    let _ = std::io::stderr().write_all(&output.stderr);

    if output.status.success() {
        return Ok(());
    }

    // Map the most common "not actually drift" failure mode (mypy not
    // installed in the chosen interpreter) to a clearer error so the
    // help-text guidance to `pip install mypy` is reliable. Python
    // surfaces this as `No module named mypy` or, for older builds,
    // `Error while finding module specification for 'mypy.stubtest'`.
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("No module named 'mypy'")
        || stderr.contains("No module named mypy")
        || stderr.contains("mypy.stubtest") && stderr.contains("ModuleNotFoundError")
    {
        return Err(miette!(
            "`{}` cannot import `mypy.stubtest` — install mypy in this interpreter \
             (`{} -m pip install mypy`) or pass `--python` to one that has it",
            args.python,
            args.python,
        ));
    }

    Err(miette!(
        "`{} -m mypy.stubtest {module}` reported drift (exit {})",
        args.python,
        output.status.code().unwrap_or(-1)
    ))
}

/// Walk `out_dir` recursively and return the Python import path of every
/// `.pyi` stub discovered there. The path is derived from the file's
/// location relative to `out_dir`: `pkg/sub/mod.pyi` becomes
/// `pkg.sub.mod`. A bare `__init__.pyi` resolves to the parent
/// directory's dotted name.
fn collect_pyi_modules(out_dir: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    walk(out_dir, out_dir, &mut out)?;
    out.sort();
    out.dedup();
    Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(miette!("cannot list '{}': {e}", dir.display())),
    };
    for entry in entries {
        let entry =
            entry.map_err(|e| miette!("cannot read entry under '{}': {e}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, out)?;
        } else if path.extension().and_then(|s| s.to_str()) == Some("pyi") {
            if let Some(module) = pyi_path_to_module(root, &path) {
                out.push(module);
            }
        }
    }
    Ok(())
}

/// Map a `.pyi` file path to its Python dotted-import name, relative to
/// the build output root. Returns `None` when the path lies outside the
/// root (shouldn't happen given the walk above) or contains a non-Unicode
/// component the Python import system would reject anyway.
fn pyi_path_to_module(root: &Path, file: &Path) -> Option<String> {
    let rel = file.strip_prefix(root).ok()?;
    let stem = rel.with_extension("");
    let mut parts: Vec<String> = stem
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(|s| s.to_owned()))
        .collect();
    // `pkg/__init__.pyi` → `pkg`; drop the trailing `__init__` component.
    if parts.last().map(|s| s == "__init__").unwrap_or(false) {
        parts.pop();
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("."))
}

fn path_separator() -> &'static str {
    if cfg!(windows) {
        ";"
    } else {
        ":"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pyi_path_to_module_handles_nested_modules() {
        let root = PathBuf::from("/build");
        let file = root.join("pkg").join("util").join("io.pyi");
        assert_eq!(
            pyi_path_to_module(&root, &file).as_deref(),
            Some("pkg.util.io")
        );
    }

    #[test]
    fn pyi_path_to_module_treats_init_as_package() {
        let root = PathBuf::from("/build");
        let file = root.join("pkg").join("__init__.pyi");
        assert_eq!(pyi_path_to_module(&root, &file).as_deref(), Some("pkg"));
    }

    #[test]
    fn pyi_path_to_module_top_level_stub() {
        let root = PathBuf::from("/build");
        let file = root.join("util.pyi");
        assert_eq!(pyi_path_to_module(&root, &file).as_deref(), Some("util"));
    }

    #[test]
    fn pyi_path_to_module_returns_none_outside_root() {
        let root = PathBuf::from("/build");
        let file = PathBuf::from("/other/util.pyi");
        assert!(pyi_path_to_module(&root, &file).is_none());
    }

    #[test]
    fn collect_pyi_modules_walks_recursively() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("pkg");
        std::fs::create_dir_all(pkg.join("sub")).unwrap();
        std::fs::write(tmp.path().join("top.pyi"), "x: int\n").unwrap();
        std::fs::write(pkg.join("__init__.pyi"), "y: int\n").unwrap();
        std::fs::write(pkg.join("sub").join("mod.pyi"), "z: int\n").unwrap();
        // Non-pyi files must be ignored.
        std::fs::write(pkg.join("mod.py"), "y = 1\n").unwrap();
        std::fs::write(tmp.path().join("README.md"), "").unwrap();

        let modules = collect_pyi_modules(tmp.path()).unwrap();
        assert_eq!(
            modules,
            vec!["pkg".to_owned(), "pkg.sub.mod".to_owned(), "top".to_owned(),]
        );
    }

    #[test]
    fn collect_pyi_modules_handles_missing_directory() {
        // A run-once-no-build invocation against a directory that doesn't
        // exist yet should produce an empty list, not an error. The caller
        // handles the empty-list case with its own user-facing message.
        let modules = collect_pyi_modules(Path::new("/no/such/path/anywhere")).unwrap();
        assert!(modules.is_empty());
    }
}
