//! `tyc debug` — launch the emitted Python under a debugger.
//!
//! Runs `tyc build` for the target project, then execs the configured
//! debugger (default: `pdb`) on the emitted entry-point `.py` file.  When
//! the debugger surfaces a frame, the file paths shown are the emitted
//! `build/*.py` paths — not the `.ty` sources.  Use `tyc trace` afterwards
//! to remap any captured tracebacks back to Typhon source via the v2
//! `.py.map` sidecars `tyc build` already produced.
//!
//! This is a deliberately thin v1: a Typhon-native source-mapping debugger
//! is a Phase-5 deliverable; the in-process pdb launcher unblocks
//! interactive stepping today without a separate install.

use std::path::PathBuf;
use std::process::Command;

use clap::Args;
use miette::{miette, Result};

use crate::commands::build::{self, BuildArgs};
use crate::config::TyphonConfig;

/// Arguments for `tyc debug`.
#[derive(Args, Debug)]
pub struct DebugArgs {
    /// Project directory (defaults to the current directory).
    #[arg(value_name = "PATH", default_value = ".")]
    pub path: PathBuf,

    /// Entry-point `.py` (relative to the build dir) to debug.
    /// Defaults to `main.py`.
    #[arg(long, value_name = "FILE", default_value = "main.py")]
    pub entry: PathBuf,

    /// Python interpreter to use (defaults to `python3`).
    #[arg(long, value_name = "PATH", default_value = "python3")]
    pub python: String,

    /// Debugger module to launch under `-m`.  Defaults to `pdb`.
    /// Common alternatives: `pdb`, `pudb`, `ipdb`, `debugpy`.
    #[arg(long, value_name = "MODULE", default_value = "pdb")]
    pub debugger: String,

    /// Extra arguments forwarded to the entry-point script after `--`.
    #[arg(last = true, value_name = "ARGS")]
    pub script_args: Vec<String>,

    /// Skip rebuilding; assume the `build/` directory is already current.
    #[arg(long)]
    pub no_build: bool,
}

pub fn run(args: DebugArgs) -> Result<()> {
    // 1. Build the project (unless --no-build).  This guarantees the
    //    emitted .py and .py.map sidecars are up to date before we step
    //    into them.
    if !args.no_build {
        build::run(BuildArgs {
            path: args.path.clone(),
            out: None,
            no_format: false,
        })?;
    }

    // 2. Resolve the build directory the same way `tyc build` does so we
    //    can locate the entry-point file.
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
    let out_dir = config_dir.join(&config.project.out);
    let entry = out_dir.join(&args.entry);

    if !entry.exists() {
        return Err(miette!(
            "entry-point '{}' does not exist; pass --entry or run `tyc build` first",
            entry.display()
        ));
    }

    // 3. Launch `<python> -m <debugger> <entry> [args...]`.
    //    The debugger inherits stdin/stdout/stderr so the user gets a
    //    fully interactive session.
    eprintln!(
        "tyc debug: launching {} -m {} '{}'",
        args.python,
        args.debugger,
        entry.display()
    );
    eprintln!(
        "tyc debug: (frames show emitted .py paths; pipe tracebacks through `tyc trace` to remap)"
    );

    let mut cmd = Command::new(&args.python);
    cmd.arg("-m").arg(&args.debugger).arg(&entry);
    cmd.args(&args.script_args);

    let status = cmd
        .status()
        .map_err(|e| miette!("cannot spawn '{}': {e}", args.python))?;

    if !status.success() {
        return Err(miette!("{} exited with {}", args.debugger, status));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(clap::Parser, Debug)]
    struct WrapDebug {
        #[command(flatten)]
        args: DebugArgs,
    }

    #[test]
    fn args_default_to_pdb_and_main_py() {
        // Sanity check that the clap defaults populate as documented.
        let parsed = <WrapDebug as clap::Parser>::try_parse_from(["debug"]).unwrap();
        assert_eq!(parsed.args.debugger, "pdb");
        assert_eq!(parsed.args.python, "python3");
        assert_eq!(parsed.args.entry, PathBuf::from("main.py"));
        assert_eq!(parsed.args.path, PathBuf::from("."));
        assert!(parsed.args.script_args.is_empty());
        assert!(!parsed.args.no_build);
    }

    #[test]
    fn missing_entry_returns_error_when_no_build() {
        let tmp = tempfile::tempdir().unwrap();
        // No build dir, --no-build skips the build, so entry lookup must fail.
        let args = DebugArgs {
            path: tmp.path().to_path_buf(),
            entry: PathBuf::from("main.py"),
            python: "python3".into(),
            debugger: "pdb".into(),
            script_args: vec![],
            no_build: true,
        };
        let err = run(args).expect_err("missing entry must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("does not exist"),
            "expected 'does not exist' error, got: {msg}"
        );
    }
}
