//! `tyc ty` — run Astral's `ty` type checker against Typhon's emitted Python.
//!
//! Typhon's own checker (`tyc check`) operates on `.ty` source. To get a
//! second opinion from a Python-native checker, build the project (optionally
//! into a temporary directory) and invoke `ty check` on the emitted `.py`
//! files.
//!
//! The integration is intentionally external: `ty` is not vendored into the
//! Typhon source tree. Users install it via `pip install ty` or
//! `uv tool install ty` and `tyc ty` finds it on `$PATH`. A different binary
//! can be selected with `--ty-bin`.

use std::path::PathBuf;
use std::process::Command;

use clap::Args;
use miette::{miette, Result};

use crate::commands::build::{self, BuildArgs};

/// Arguments for `tyc ty`.
#[derive(Args, Debug)]
pub struct TyArgs {
    /// Project directory (defaults to current directory).
    #[arg(value_name = "PATH", default_value = ".")]
    pub path: PathBuf,

    /// Build into this directory before running `ty`. If omitted, a temporary
    /// directory is used and removed when `tyc ty` exits.
    ///
    /// Useful in CI to keep the emitted output for later inspection:
    /// `tyc ty --out build/`.
    #[arg(long, short = 'o', value_name = "DIR")]
    pub out: Option<PathBuf>,

    /// Path to the `ty` executable. Defaults to `ty` (found via `$PATH`).
    #[arg(long, value_name = "BIN", default_value = "ty")]
    pub ty_bin: String,

    /// Skip the build step and run `ty` against an existing output directory.
    /// Requires `--out` so the directory is unambiguous.
    #[arg(long)]
    pub no_build: bool,

    /// Extra arguments forwarded to `ty check` verbatim.
    #[arg(last = true)]
    pub ty_args: Vec<String>,
}

pub fn run(args: TyArgs) -> Result<()> {
    if args.no_build && args.out.is_none() {
        return Err(miette!(
            "--no-build requires --out so the output directory is known"
        ));
    }

    // Resolve / create the directory `ty check` will scan.
    //
    // A relative `--out` is anchored to the project path (matching
    // `tyc build`'s behaviour) — otherwise the build and the subsequent
    // `ty check` would disagree about where the artefacts live when `tyc ty`
    // is invoked from a different working directory than the project.
    let (out_dir, _tempdir_guard) = match (&args.out, args.no_build) {
        (Some(dir), _) => {
            let resolved = if dir.is_absolute() {
                dir.clone()
            } else {
                args.path.join(dir)
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
        build::run(BuildArgs {
            path: args.path.clone(),
            out: Some(out_dir.clone()),
            no_format: false,
        })?;
    }

    // Anchor the subprocess at the project root so `ty` discovers any
    // `pyproject.toml` / `ty.toml` / virtualenv from the project, not from
    // whatever directory the user happened to invoke `tyc ty` from. The
    // output directory is passed as an absolute path so this doesn't
    // double-anchor it.
    let mut cmd = Command::new(&args.ty_bin);
    cmd.current_dir(&args.path);
    cmd.arg("check").arg(&out_dir);
    for extra in &args.ty_args {
        cmd.arg(extra);
    }

    let status = cmd.status().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => miette!(
            "`{}` not found on PATH — install Astral's `ty` (e.g. `pip install ty` \
             or `uv tool install ty`) or pass --ty-bin to point at your install",
            args.ty_bin,
        ),
        _ => miette!("failed to run `{} check`: {e}", args.ty_bin),
    })?;

    if !status.success() {
        return Err(miette!(
            "`{}` reported type errors (exit {})",
            args.ty_bin,
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_build_without_out_errors() {
        let result = run(TyArgs {
            path: PathBuf::from("."),
            out: None,
            ty_bin: "ty".into(),
            no_build: true,
            ty_args: vec![],
        });
        assert!(result.is_err());
        let msg = format!("{:?}", result.unwrap_err());
        assert!(msg.contains("--no-build requires --out"));
    }

    #[test]
    fn relative_out_is_resolved_against_project_path() {
        // Regression for a path-mismatch bug: a relative `--out` must be
        // anchored to the project path so the build and the subsequent
        // `ty check` invocation agree on where the artefacts live, even when
        // `tyc ty` is invoked from a different working directory.
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tmp.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("main.ty"), "let x: int = 1\n").unwrap();
        std::fs::write(
            tmp.path().join("typhon.toml"),
            "[project]\nname = \"test\"\n",
        )
        .unwrap();

        let result = run(TyArgs {
            path: tmp.path().to_path_buf(),
            out: Some(PathBuf::from("build")),
            ty_bin: "ty-definitely-does-not-exist-12345".into(),
            no_build: false,
            ty_args: vec![],
        });
        assert!(result.is_err(), "missing ty binary should error");

        // The build should have written into <tmp>/build, not ./build.
        let expected = tmp.path().join("build");
        assert!(
            expected.exists(),
            "build output should be at {} after `tyc ty --out build`",
            expected.display(),
        );
    }

    #[test]
    fn missing_ty_binary_reports_install_hint() {
        // Build a tiny throwaway project, point at a binary that doesn't exist,
        // and confirm the user-facing error mentions how to install `ty`.
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tmp.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("main.ty"), "let x: int = 1\n").unwrap();
        std::fs::write(
            tmp.path().join("typhon.toml"),
            "[project]\nname = \"test\"\n",
        )
        .unwrap();

        let out = tmp.path().join("build");
        let result = run(TyArgs {
            path: tmp.path().to_path_buf(),
            out: Some(out),
            ty_bin: "ty-definitely-does-not-exist-12345".into(),
            no_build: false,
            ty_args: vec![],
        });
        assert!(result.is_err(), "missing binary should error");
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("not found on PATH") && msg.contains("pip install ty"),
            "error should hint at install: {msg}",
        );
    }
}
