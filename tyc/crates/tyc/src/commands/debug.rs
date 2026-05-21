//! `tyc debug` — launch the emitted Python under a debugger.
//!
//! Runs `tyc build` for the target project, then execs the configured
//! debugger (default: `pdb`) on the emitted entry-point `.py` file.  When
//! the debugger surfaces a frame, the file paths shown are the emitted
//! `build/*.py` paths — not the `.ty` sources.  Use `tyc trace` afterwards
//! to remap any captured tracebacks back to Typhon source via the v2
//! `.py.map` sidecars `tyc build` already produced.
//!
//! ## Typhon-line breakpoints (`--break`)
//!
//! Each `--break <ty-file>:<line>` flag is translated to a `break` pdb
//! command **before** pdb is launched.  The translator opens the
//! corresponding `.py.map` sidecar, reads its v2 `lines` table (each
//! entry maps a 0-indexed Python output line to a 1-indexed Typhon
//! source line) and returns the first Python line that originated from
//! the requested Typhon line.  The resulting `break <py-file>:<py-line>`
//! commands are passed to pdb via `-c`, which pdb evaluates before
//! starting execution.
//!
//! ### Still missing (deferred)
//!
//! - v1 maps **entry** breakpoints only; v2 (deferred) will map
//!   step-throughs back to Typhon lines via pdb hooks (e.g. an
//!   inheriting `Pdb` subclass that displays the `.ty` location each
//!   time the program pauses).
//! - When a `--break` spec cannot be resolved (missing sidecar, line
//!   unmapped) the launcher prints a warning to stderr and continues
//!   without that breakpoint rather than aborting.

use std::path::{Path, PathBuf};
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

    /// Break at a Typhon source location, translated via `.py.map`.
    /// Format: `<ty-file>:<line>` (e.g. `src/main.ty:42`). Repeatable.
    #[arg(long = "break", value_name = "TY:LINE", action = clap::ArgAction::Append)]
    pub breakpoints: Vec<String>,

    /// Extra arguments forwarded to the entry-point script after `--`.
    #[arg(last = true, value_name = "ARGS")]
    pub script_args: Vec<String>,

    /// Skip rebuilding; assume the `build/` directory is already current.
    #[arg(long)]
    pub no_build: bool,
}

/// A parsed `<ty-file>:<line>` breakpoint specification.
#[derive(Debug, PartialEq, Eq, Clone)]
pub(crate) struct BreakpointSpec {
    pub ty_file: PathBuf,
    pub line: u32,
}

/// Parse a `<ty-file>:<line>` string.  The line number is parsed from
/// the segment after the *last* `:`, so Windows-style drive letters
/// (`C:\foo.ty:10`) and POSIX paths (`src/foo.ty:10`) both round-trip.
pub(crate) fn parse_breakpoint_spec(spec: &str) -> Result<BreakpointSpec, String> {
    let (file_part, line_part) = match spec.rsplit_once(':') {
        Some(pair) => pair,
        None => return Err(format!("breakpoint '{spec}' missing ':<line>' suffix")),
    };
    if file_part.is_empty() {
        return Err(format!("breakpoint '{spec}' has empty file"));
    }
    let line: u32 = line_part
        .parse()
        .map_err(|_| format!("breakpoint '{spec}' line '{line_part}' is not a positive integer"))?;
    if line == 0 {
        return Err(format!("breakpoint '{spec}' line must be >= 1"));
    }
    Ok(BreakpointSpec {
        ty_file: PathBuf::from(file_part),
        line,
    })
}

/// Given the JSON body of a v2 `.py.map`, return the first Python line
/// (1-indexed) that originated from `ty_line` (1-indexed).  Returns
/// `None` when the Typhon line never appears in the table.
///
/// The mapping format is intentionally tolerant: we extract the `lines`
/// array by string-scanning rather than pulling in a full JSON dependency
/// at this layer.  When the sidecar is malformed we return `None` and
/// let the caller fall back to a warning.
pub(crate) fn lookup_py_line_in_map(map_body: &str, ty_line: u32) -> Option<u32> {
    let lines_key = "\"lines\":[";
    let start = map_body.find(lines_key)? + lines_key.len();
    let end = map_body[start..].find(']')? + start;
    let body = &map_body[start..end];
    for (i, raw) in body.split(',').enumerate() {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let n: u32 = raw.parse().ok()?;
        if n == ty_line {
            // Python output lines are 0-indexed in the table; pdb expects 1-indexed.
            return Some((i as u32) + 1);
        }
    }
    None
}

/// Translate a Typhon breakpoint into a pdb `break <file>:<line>` command.
///
/// `src_dir` is the project source root (where `.ty` lives); `out_dir`
/// is the build output (where `.py` and `.py.map` live).  When the spec
/// cannot be resolved the function returns `Err(msg)` describing why.
pub(crate) fn translate_breakpoint(
    spec: &BreakpointSpec,
    src_dir: &Path,
    out_dir: &Path,
) -> Result<(PathBuf, u32), String> {
    // The emitter computes `rel = path.strip_prefix(src_dir)` and then
    // writes `out_dir.join(rel).with_extension("py")`.  Mirror that:
    //   - Absolute spec: strip src_dir if it lives there.
    //   - Relative spec: first try as-is, then try stripping the
    //     `<src>` prefix the user likely typed (e.g. `src/foo.ty`).
    let src_name = src_dir.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let rel: PathBuf = if spec.ty_file.is_absolute() {
        spec.ty_file
            .strip_prefix(src_dir)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| spec.ty_file.clone())
    } else if !src_name.is_empty() && spec.ty_file.starts_with(src_name) {
        spec.ty_file
            .strip_prefix(src_name)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| spec.ty_file.clone())
    } else {
        spec.ty_file.clone()
    };
    // The emitter writes `<src>/foo.ty` to `<out>/foo.py` + `<out>/foo.py.map`.
    let py_path = out_dir.join(&rel).with_extension("py");
    let map_path = out_dir.join(&rel).with_extension("py.map");
    if !map_path.exists() {
        return Err(format!(
            "no .py.map sidecar for '{}' (expected '{}')",
            spec.ty_file.display(),
            map_path.display()
        ));
    }
    let map_body = std::fs::read_to_string(&map_path)
        .map_err(|e| format!("cannot read '{}': {e}", map_path.display()))?;
    let py_line = lookup_py_line_in_map(&map_body, spec.line).ok_or_else(|| {
        format!(
            "Typhon line {} in '{}' does not map to any Python line",
            spec.line,
            spec.ty_file.display()
        )
    })?;
    Ok((py_path, py_line))
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
            check: false,
            no_sync: false,
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
    let src_dir = config_dir.join(&config.project.src);
    let entry = out_dir.join(&args.entry);

    if !entry.exists() {
        return Err(miette!(
            "entry-point '{}' does not exist; pass --entry or run `tyc build` first",
            entry.display()
        ));
    }

    // 3. Translate any `--break <ty:line>` flags into pdb pre-commands.
    let mut pdb_cmds: Vec<String> = Vec::new();
    for raw in &args.breakpoints {
        let spec = match parse_breakpoint_spec(raw) {
            Ok(s) => s,
            Err(msg) => {
                eprintln!("tyc debug: skipping breakpoint: {msg}");
                continue;
            }
        };
        match translate_breakpoint(&spec, &src_dir, &out_dir) {
            Ok((py_path, py_line)) => {
                let cmd = format!("break {}:{}", py_path.display(), py_line);
                eprintln!(
                    "tyc debug: {} -> {} ({})",
                    raw,
                    cmd,
                    py_path.file_name().and_then(|n| n.to_str()).unwrap_or("?")
                );
                pdb_cmds.push(cmd);
            }
            Err(msg) => {
                eprintln!("tyc debug: cannot translate '{raw}': {msg}");
            }
        }
    }

    // 4. Launch `<python> -m <debugger> [-c "break ..."]* <entry> [args...]`.
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
    cmd.arg("-m").arg(&args.debugger);
    for c in &pdb_cmds {
        cmd.arg("-c").arg(c);
    }
    cmd.arg(&entry);
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
        assert!(parsed.args.breakpoints.is_empty());
    }

    #[test]
    fn args_collect_repeated_break_flags() {
        let parsed = <WrapDebug as clap::Parser>::try_parse_from([
            "debug",
            "--break",
            "src/main.ty:10",
            "--break",
            "src/lib.ty:25",
        ])
        .unwrap();
        assert_eq!(
            parsed.args.breakpoints,
            vec!["src/main.ty:10".to_string(), "src/lib.ty:25".to_string()]
        );
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
            breakpoints: vec![],
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

    #[test]
    fn parse_breakpoint_spec_accepts_file_line_form() {
        let s = parse_breakpoint_spec("src/main.ty:42").unwrap();
        assert_eq!(s.ty_file, PathBuf::from("src/main.ty"));
        assert_eq!(s.line, 42);
    }

    #[test]
    fn parse_breakpoint_spec_rejects_invalid_form() {
        assert!(parse_breakpoint_spec("src/main.ty").is_err());
        assert!(parse_breakpoint_spec("src/main.ty:abc").is_err());
        assert!(parse_breakpoint_spec(":42").is_err());
        assert!(parse_breakpoint_spec("src/main.ty:0").is_err());
    }

    #[test]
    fn lookup_py_line_returns_first_match() {
        // lines=[1,1,5,5,5] means py lines 1-2 came from ty line 1 and py
        // lines 3-5 came from ty line 5.  Looking up ty line 5 returns the
        // first matching py line (3, 1-indexed).
        let body =
            r#"{"version":2,"source":"main.ty","line_strategy":"table","lines":[1,1,5,5,5]}"#;
        assert_eq!(lookup_py_line_in_map(body, 1), Some(1));
        assert_eq!(lookup_py_line_in_map(body, 5), Some(3));
        assert_eq!(lookup_py_line_in_map(body, 9), None);
    }

    #[test]
    fn translate_breakpoint_via_pymap_returns_python_line() {
        // Set up a synthetic project layout:
        //   tmp/src/foo.ty             (just for path realism)
        //   tmp/build/foo.py
        //   tmp/build/foo.py.map       (lines=[1,1,5,5,5])
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let out = tmp.path().join("build");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(src.join("foo.ty"), "let x = 1\n").unwrap();
        std::fs::write(out.join("foo.py"), "x = 1\n").unwrap();
        std::fs::write(
            out.join("foo.py.map"),
            r#"{"version":2,"source":"foo.ty","line_strategy":"table","lines":[1,1,5,5,5]}"#,
        )
        .unwrap();
        let spec = parse_breakpoint_spec("src/foo.ty:5").unwrap();
        let (py_path, py_line) = translate_breakpoint(&spec, &src, &out).unwrap();
        // The `src/` prefix is stripped to match the emitter's layout.
        assert_eq!(py_path, out.join("foo.py"));
        assert_eq!(py_line, 3);
    }

    #[test]
    fn translate_breakpoint_errors_when_pymap_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let out = tmp.path().join("build");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        let spec = parse_breakpoint_spec("missing.ty:1").unwrap();
        let err = translate_breakpoint(&spec, &src, &out).unwrap_err();
        assert!(err.contains("no .py.map"), "got: {err}");
    }
}
