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
//! ## Typhon-aware presentation layer (`TyphonPdb`)
//!
//! When the default `pdb` debugger is used (the common case), the
//! launcher writes a small Python wrapper that subclasses `pdb.Pdb`
//! and overrides `print_stack_entry` to print `[ty] <src>:<line>`
//! alongside the standard `.py` frame on every pause (entry,
//! breakpoint, step, exception). The wrapper eagerly loads every
//! `.py.map` sidecar in the build directory on startup so the lookup
//! at pause time is a dict + list dereference. Pass `--raw-pdb` to
//! opt out and launch `python -m pdb` directly.
//!
//! ### Still missing (deferred)
//!
//! - When a `--break` spec cannot be resolved (missing sidecar, line
//!   unmapped) the launcher prints a warning to stderr and continues
//!   without that breakpoint rather than aborting.
//! - The pdb command-line UI still shows `.py` paths in the prompt;
//!   only the post-pause attribution line is rewritten. A future v3
//!   layer could translate `where` / `list` output as well.

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

    /// Disable the Typhon-aware pdb subclass. By default `tyc debug`
    /// generates a small wrapper that surfaces the `.ty:line` location
    /// on every pause (entry, breakpoint, step, exception). Pass
    /// `--raw-pdb` to launch `python3 -m pdb` directly with no extra
    /// presentation layer. The wrapper requires `--debugger pdb`
    /// (the default); selecting a different debugger automatically
    /// disables it.
    #[arg(long)]
    pub raw_pdb: bool,
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

    // 4. Decide whether to launch the Typhon-aware wrapper or fall
    //    back to plain `python -m <debugger>`.
    let use_wrapper = !args.raw_pdb && args.debugger == "pdb";

    let mut wrapper_guard: Option<tempfile::NamedTempFile> = None;

    let mut cmd = Command::new(&args.python);
    if use_wrapper {
        let wrapper_path = write_typhon_pdb_wrapper(&out_dir, &entry, &pdb_cmds)
            .map_err(|e| miette!("cannot write Typhon pdb wrapper: {e}"))?;
        eprintln!(
            "tyc debug: launching {} '{}' (Typhon-aware pdb)",
            args.python,
            wrapper_path.path().display()
        );
        eprintln!(
            "tyc debug: pauses will surface as `[ty] <src>/file.ty:line` next to the `.py` frame"
        );
        cmd.arg(wrapper_path.path());
        cmd.args(&args.script_args);
        wrapper_guard = Some(wrapper_path);
    } else {
        eprintln!(
            "tyc debug: launching {} -m {} '{}'",
            args.python,
            args.debugger,
            entry.display()
        );
        eprintln!(
            "tyc debug: (frames show emitted .py paths; pipe tracebacks through `tyc trace` to remap)"
        );
        cmd.arg("-m").arg(&args.debugger);
        for c in &pdb_cmds {
            cmd.arg("-c").arg(c);
        }
        cmd.arg(&entry);
        cmd.args(&args.script_args);
    }

    let status = cmd
        .status()
        .map_err(|e| miette!("cannot spawn '{}': {e}", args.python))?;

    // Keep the wrapper alive until the debugger exits.
    drop(wrapper_guard);

    if !status.success() {
        return Err(miette!("{} exited with {}", args.debugger, status));
    }
    Ok(())
}

/// Write a one-shot Python wrapper script to a tempfile that subclasses
/// `pdb.Pdb`, loads the project's `.py.map` sidecars, and prints the
/// originating `.ty:line` next to every paused frame.
///
/// The wrapper returns a [`tempfile::NamedTempFile`]. Caller is
/// responsible for keeping it alive until the child process exits —
/// dropping the handle deletes the file.
fn write_typhon_pdb_wrapper(
    out_dir: &Path,
    entry: &Path,
    breakpoint_cmds: &[String],
) -> std::io::Result<tempfile::NamedTempFile> {
    use std::io::Write;
    let entry_str = entry.display().to_string();
    let map_dir = out_dir.display().to_string();
    // Quoting strategy: emit each path as a Python double-quoted
    // string literal via Rust's `{:?}` formatter. Rust's debug
    // format escapes `\` and `"` per the Rust string spec, which
    // happens to be a valid subset of Python's string-literal
    // escaping — so a Windows path like `C:\foo\bar.py` round-trips
    // as `"C:\\foo\\bar.py"`. The previous version used a raw
    // `r"..."` prefix, which silently double-escaped backslashes
    // (file lookups would then miss on Windows) and could not end
    // in a backslash. FINDINGS — gemini review of PR #105.
    let entry_lit = format!("{:?}", entry_str);
    let map_dir_lit = format!("{:?}", map_dir);
    let break_cmds: String = breakpoint_cmds
        .iter()
        .map(|c| format!("    {:?},\n", c))
        .collect();

    let body = format!(
        r#"# Generated by `tyc debug` — Typhon-aware pdb wrapper.
#
# Subclasses pdb.Pdb so every pause (entry, breakpoint, step, exception)
# prints the originating .ty:line alongside the emitted .py frame. Maps
# are loaded eagerly from the build directory so the lookup at pause
# time is a dict-and-vec dereference, not a filesystem call.
import json
import os
import pdb
import sys
import runpy


_ENTRY = {entry_lit}
_BUILD_DIR = {map_dir_lit}
_BREAK_CMDS = [
{break_cmds}]


def _load_maps(build_dir):
    """Return dict[py_path -> (ty_source, lines_table)].

    Loads every `*.py.map` JSON sidecar under `build_dir` recursively.
    Malformed sidecars are skipped silently — the worst case is that
    a frame's `[ty]` annotation is missing, not that the debugger
    fails to launch.
    """
    out = {{}}
    for root, _dirs, files in os.walk(build_dir):
        for name in files:
            if not name.endswith(".py.map"):
                continue
            map_path = os.path.join(root, name)
            try:
                with open(map_path, "r", encoding="utf-8") as f:
                    body = json.load(f)
            except (OSError, ValueError):
                continue
            source = body.get("source")
            if not isinstance(source, str):
                continue
            lines = body.get("lines")
            if not isinstance(lines, list):
                lines = []
            py_path = map_path[:-4]  # strip ".map" → "foo.py"
            out[os.path.abspath(py_path)] = (source, [int(x) for x in lines])
    return out


_MAPS = _load_maps(_BUILD_DIR)


def _attribute(filename, lineno):
    """Look the frame's (.py, py_line) up; return ".ty:ty_line" or None."""
    if not filename:
        return None
    abs_path = os.path.abspath(filename)
    entry = _MAPS.get(abs_path)
    if entry is None:
        return None
    source, lines = entry
    idx = lineno - 1
    if 0 <= idx < len(lines):
        ty_line = lines[idx]
    else:
        ty_line = lineno
    return "{{}}:{{}}".format(source, ty_line)


class TyphonPdb(pdb.Pdb):
    """pdb.Pdb subclass that surfaces the .ty:line for the current frame."""

    def print_stack_entry(self, frame_lineno, prompt_prefix=pdb.line_prefix):
        super().print_stack_entry(frame_lineno, prompt_prefix)
        frame, lineno = frame_lineno
        attribution = _attribute(frame.f_code.co_filename, lineno)
        if attribution is not None:
            self.message("[ty] {{}}".format(attribution))


def _main():
    p = TyphonPdb()
    for cmd in _BREAK_CMDS:
        p.rcLines.append(cmd)
    # Use runpy so the entry script behaves like `python entry.py`
    # (its `__name__` is `__main__`, its `__file__` is `_ENTRY`, etc.).
    sys.argv = [_ENTRY] + sys.argv[1:]
    try:
        p.run("runpy.run_path({{!r}}, run_name='__main__')".format(_ENTRY),
              globals={{'runpy': runpy}}, locals=None)
    except SystemExit:
        raise
    except BaseException:
        p.interaction(None, sys.exc_info()[2])


if __name__ == "__main__":
    _main()
"#,
        entry_lit = entry_lit,
        map_dir_lit = map_dir_lit,
        break_cmds = break_cmds,
    );

    let mut tmp = tempfile::Builder::new()
        .prefix("typhon-pdb-")
        .suffix(".py")
        .tempfile()?;
    tmp.write_all(body.as_bytes())?;
    tmp.flush()?;
    Ok(tmp)
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
            raw_pdb: false,
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
    fn typhon_pdb_wrapper_includes_entry_and_maps_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("build");
        std::fs::create_dir_all(&out).unwrap();
        let entry = out.join("main.py");
        std::fs::write(&entry, "print('hi')\n").unwrap();
        let cmds = vec!["break main.py:1".to_owned()];
        let wrapper = write_typhon_pdb_wrapper(&out, &entry, &cmds).unwrap();
        let body = std::fs::read_to_string(wrapper.path()).unwrap();
        assert!(body.contains("class TyphonPdb(pdb.Pdb)"));
        assert!(body.contains("[ty]"));
        assert!(body.contains("break main.py:1"));
        assert!(body.contains(entry.to_string_lossy().as_ref()));
    }

    #[test]
    fn typhon_pdb_wrapper_python_parses() {
        // The generated wrapper must be a syntactically valid Python
        // module; run `python3 -m py_compile` on it. The test is
        // skipped silently when no python3 is available so the
        // workspace test suite stays portable.
        let python3 = std::process::Command::new("python3")
            .arg("-c")
            .arg("import sys; sys.exit(0)")
            .status();
        if python3.is_err() || !python3.unwrap().success() {
            eprintln!("skipping typhon_pdb_wrapper_python_parses: no python3 on PATH");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("build");
        std::fs::create_dir_all(&out).unwrap();
        let entry = out.join("main.py");
        std::fs::write(&entry, "print('hi')\n").unwrap();
        let wrapper = write_typhon_pdb_wrapper(&out, &entry, &[]).unwrap();
        let status = std::process::Command::new("python3")
            .arg("-m")
            .arg("py_compile")
            .arg(wrapper.path())
            .status()
            .expect("py_compile spawn");
        assert!(
            status.success(),
            "generated wrapper failed py_compile: {}",
            std::fs::read_to_string(wrapper.path()).unwrap_or_default()
        );
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
