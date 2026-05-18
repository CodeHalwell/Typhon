//! `tyc run` — build the project and execute the emitted Python.
//!
//! Hides the `tyc build && python build/main.py` two-step behind a single
//! command, mirroring how `tsx`/`ts-node` hide the TypeScript→JavaScript
//! compile step.
//!
//! Two output modes:
//!
//! * **Persistent (default).** Builds into the configured `out` dir
//!   (default `build/`).  Subsequent runs hit the incremental Salsa
//!   cache, and `.py.map` sidecars stay on disk so a post-crash
//!   `tyc trace` can map frames back to `.ty`.
//! * **`--temp` (`-t`).** Builds into a fresh `tempfile::tempdir()` that
//!   is removed when the process exits.  The "tyx in-memory" mode — no
//!   project pollution, ideal for quick one-shot iteration.  Trades the
//!   incremental cache and on-disk source map for a clean tree.
//!
//! Typhon does not have a separate VM: every `.ty` file lowers to clean
//! CPython.  `tyc run` is a UX shortcut, not a new execution model.
//!
//! Exit-code semantics: when the build succeeds and the script launches,
//! `tyc run` exits with the script's own exit code (via `process::exit`)
//! so shell pipelines see the child's status verbatim.  Build failures
//! and spawn failures surface as normal miette errors with exit code 1.

use std::path::PathBuf;
use std::process::Command;

use clap::Args;
use miette::{miette, Result};
use tempfile::TempDir;

use crate::commands::build::{self, BuildArgs};
use crate::config::TyphonConfig;

/// Arguments for `tyc run`.
#[derive(Args, Debug)]
pub struct RunArgs {
    /// Project directory (defaults to the current directory).
    #[arg(value_name = "PATH", default_value = ".")]
    pub path: PathBuf,

    /// Entry-point `.py` (relative to the build dir) to execute.
    /// Defaults to `main.py`.
    #[arg(long, value_name = "FILE", default_value = "main.py")]
    pub entry: PathBuf,

    /// Python interpreter to use (defaults to `python3`).
    #[arg(long, value_name = "PATH", default_value = "python3")]
    pub python: String,

    /// Build into a temporary directory that is deleted when the process
    /// exits, instead of the configured `out` dir.  No build artifacts
    /// persist on disk — the "tyx in-memory" mode.  Implies a fresh
    /// build every invocation, so the incremental cache and on-disk
    /// `.py.map` sidecars are unavailable.
    #[arg(long, short = 't', conflicts_with = "no_build")]
    pub temp: bool,

    /// Skip rebuilding; assume the `build/` directory is already current.
    /// Incompatible with `--temp`.
    #[arg(long)]
    pub no_build: bool,

    /// Extra arguments forwarded to the entry-point script after `--`.
    #[arg(last = true, value_name = "ARGS")]
    pub script_args: Vec<String>,
}

pub fn run(args: RunArgs) -> Result<()> {
    // 1. Decide where build outputs go.  In `--temp` mode we own a
    //    TempDir guard whose Drop removes the directory; we keep it
    //    alive across the child process by binding it to a local.
    let (out_dir, _tmp_guard): (PathBuf, Option<TempDir>) = if args.temp {
        let tmp = tempfile::Builder::new()
            .prefix("tyc-run-")
            .tempdir()
            .map_err(|e| miette!("cannot create temp directory: {e}"))?;
        (tmp.path().to_path_buf(), Some(tmp))
    } else {
        // Resolve the persistent `out` dir the same way `tyc build` does
        // so the entry-point lookup matches what was just emitted.
        let project_root = args
            .path
            .canonicalize()
            .map_err(|e| miette!("cannot resolve path '{}': {}", args.path.display(), e))?;
        let (config_dir, config) = match TyphonConfig::load(&project_root) {
            Ok(Some((toml_path, cfg))) => {
                let dir = toml_path
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| project_root.clone());
                (dir, cfg)
            }
            Ok(None) => (project_root.clone(), TyphonConfig::default()),
            Err(e) => return Err(miette!("{e}")),
        };
        (config_dir.join(&config.project.out), None)
    };

    // 2. Build the project unless --no-build was passed.  When --temp is
    //    set we always build (clap's conflicts_with already rejected
    //    --no-build + --temp).  Pass an explicit `out` only for --temp;
    //    leaving it None lets `tyc build` pick its own configured dir.
    if !args.no_build {
        build::run(BuildArgs {
            path: args.path.clone(),
            out: if args.temp { Some(out_dir.clone()) } else { None },
            no_format: false,
        })?;
    }

    let entry = out_dir.join(&args.entry);
    if !entry.exists() {
        return Err(miette!(
            "entry-point '{}' does not exist; pass --entry or drop --no-build",
            entry.display()
        ));
    }

    // 3. Spawn `<python> <entry> [args...]`.  Python prepends the
    //    script's directory to sys.path, so `import typhon_runtime`
    //    resolves against the build dir without any PYTHONPATH plumbing.
    let mut cmd = Command::new(&args.python);
    cmd.arg(&entry);
    cmd.args(&args.script_args);

    let status = cmd
        .status()
        .map_err(|e| miette!("cannot spawn '{}': {e}", args.python))?;

    // 4. Propagate the child's exit code verbatim, but drop the TempDir
    //    guard first so its Drop runs — process::exit skips destructors.
    let code = status.code().unwrap_or(1);
    drop(_tmp_guard);
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(clap::Parser, Debug)]
    struct WrapRun {
        #[command(flatten)]
        args: RunArgs,
    }

    #[test]
    fn args_default_to_python3_and_main_py() {
        let parsed = <WrapRun as clap::Parser>::try_parse_from(["run"]).unwrap();
        assert_eq!(parsed.args.python, "python3");
        assert_eq!(parsed.args.entry, PathBuf::from("main.py"));
        assert_eq!(parsed.args.path, PathBuf::from("."));
        assert!(parsed.args.script_args.is_empty());
        assert!(!parsed.args.no_build);
        assert!(!parsed.args.temp);
    }

    #[test]
    fn script_args_pass_through_after_double_dash() {
        let parsed = <WrapRun as clap::Parser>::try_parse_from([
            "run", "--", "--flag", "value", "positional",
        ])
        .unwrap();
        assert_eq!(
            parsed.args.script_args,
            vec!["--flag".to_string(), "value".into(), "positional".into()]
        );
    }

    #[test]
    fn temp_flag_parses_with_short_alias() {
        let parsed = <WrapRun as clap::Parser>::try_parse_from(["run", "-t"]).unwrap();
        assert!(parsed.args.temp);

        let parsed = <WrapRun as clap::Parser>::try_parse_from(["run", "--temp"]).unwrap();
        assert!(parsed.args.temp);
    }

    #[test]
    fn temp_and_no_build_are_mutually_exclusive() {
        let err = <WrapRun as clap::Parser>::try_parse_from(["run", "--temp", "--no-build"])
            .expect_err("--temp + --no-build must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("cannot be used with") || msg.contains("conflicts"),
            "expected a conflict error, got: {msg}"
        );
    }

    #[test]
    fn missing_entry_returns_error_when_no_build() {
        let tmp = tempfile::tempdir().unwrap();
        let args = RunArgs {
            path: tmp.path().to_path_buf(),
            entry: PathBuf::from("main.py"),
            python: "python3".into(),
            temp: false,
            no_build: true,
            script_args: vec![],
        };
        let err = run(args).expect_err("missing entry must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("does not exist"),
            "expected 'does not exist' error, got: {msg}"
        );
    }
}
