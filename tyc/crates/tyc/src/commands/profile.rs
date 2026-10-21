//! `tyc profile` — find the hot functions in a Typhon program.
//!
//! Two modes, both on top of the standard `tyc build` pipeline:
//!
//! * **Sampling** (the default on `[python] target` 3.15 and later). Runs the
//!   built program under CPython 3.15's Tachyon sampler
//!   (`python -m profiling.sampling`, PEP 799). Nothing is instrumented, so
//!   methods, nested functions, lambdas and coroutines all show up. The
//!   collapsed stacks Tachyon writes are mapped back through the build's
//!   `.py.map` sidecars, so every frame names its `.ty` file and line, and
//!   saved as `typhon-profile.collapsed` (the input format of speedscope,
//!   inferno and `flamegraph.pl`). A self/total summary is printed.
//! * **Instrumenting** (the default below 3.15, or `--instrument`).
//!   Post-processes each emitted `.py` file to wrap every top-level function
//!   in a lightweight timing decorator that accumulates wall-clock samples
//!   into a process-wide registry. The registry is flushed to
//!   `typhon-profile.json` on interpreter shutdown via an `atexit` hook
//!   injected into each module. This mode does not run the program.
//!
//! Use this output to find the hottest functions before promoting them with
//! `@pure(memo=True)` or `@memo`, or before reaching for PGO.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Args;
use miette::{miette, Result};

use crate::commands::build;

/// Arguments for `tyc profile`.
#[derive(Args, Debug)]
pub struct ProfileArgs {
    /// Project directory (defaults to current directory).
    #[arg(value_name = "PATH", default_value = ".")]
    pub path: PathBuf,

    /// Output directory override forwarded to `tyc build`.
    #[arg(long, short = 'o', value_name = "DIR")]
    pub out: Option<PathBuf>,

    /// Run the program under CPython 3.15's sampling profiler (Tachyon) and
    /// map its stacks back to `.ty` lines. Default on 3.15+ targets.
    #[arg(long, conflicts_with = "instrument")]
    pub sample: bool,

    /// Instrument top-level functions with timing decorators instead of
    /// sampling (the pre-3.15 behaviour). Does not run the program.
    #[arg(long)]
    pub instrument: bool,

    /// Entry-point `.py` inside the build directory (sampling mode).
    #[arg(long, value_name = "FILE", default_value = "main.py")]
    pub entry: String,

    /// Interpreter to sample under (default: the project's `.venv`, then
    /// `python3.<minor>` for the target). Must be CPython 3.15 or later.
    #[arg(long, value_name = "PYTHON")]
    pub python: Option<String>,

    /// Sampling rate passed to Tachyon, e.g. `1khz` (its default) or `10khz`.
    #[arg(long, value_name = "RATE")]
    pub rate: Option<String>,

    /// What a sample counts: wall-clock time, CPU time, time holding the GIL,
    /// or time handling an exception.
    #[arg(long, value_name = "MODE", value_parser = ["wall", "cpu", "gil", "exception"])]
    pub mode: Option<String>,

    /// Rebuild stacks from asyncio tasks, so awaiting coroutines appear under
    /// the task that awaits them. Wall-clock mode only, and not with
    /// `--all-threads` (Tachyon's rule).
    #[arg(long, conflicts_with = "all_threads")]
    pub async_aware: bool,

    /// Sample every thread, not only the main one.
    #[arg(long)]
    pub all_threads: bool,

    /// Rows in the printed summary.
    #[arg(long, value_name = "N", default_value_t = 15)]
    pub top: usize,

    /// Arguments passed to the profiled program (after `--`).
    #[arg(last = true, value_name = "ARGS")]
    pub script_args: Vec<String>,
}

pub fn run(args: ProfileArgs) -> Result<()> {
    // First produce a normal build; the profiler post-processes the output
    // rather than re-implementing the pipeline.
    build::run(build::BuildArgs {
        path: args.path.clone(),
        out: args.out.clone(),
        no_format: false,
        check: false,
        no_sync: false,
        with_ty: false,
        optimise: false,
        source_label: None,
    })?;

    // Discover the build directory.  We mirror the resolution rules from
    // `build::run`: explicit `--out`, else read `[project] out` from
    // typhon.toml under the project root, else default to `build`.
    let project_root = args
        .path
        .canonicalize()
        .map_err(|e| miette!("cannot resolve path '{}': {}", args.path.display(), e))?;
    let (config_dir, config) = match crate::config::TyphonConfig::load(&project_root) {
        Ok(Some((toml_path, cfg))) => {
            let dir = toml_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| project_root.clone());
            (dir, cfg)
        }
        Ok(None) => (project_root.clone(), crate::config::TyphonConfig::default()),
        Err(e) => return Err(miette!("{e}")),
    };
    let out_dir = match args.out.clone() {
        Some(out) => {
            if out.is_absolute() {
                out
            } else {
                config_dir.join(out)
            }
        }
        None => config_dir.join(&config.project.out),
    };

    if !out_dir.exists() {
        return Err(miette!(
            "build output '{}' does not exist; did `tyc build` succeed?",
            out_dir.display()
        ));
    }

    let sample = args.sample
        || (!args.instrument
            && crate::config::parse_python_target(&config.python.target)
                .is_some_and(|v| v >= (3, 15)));
    if sample {
        return run_sampling(&args, &config_dir, &out_dir);
    }

    let mut instrumented = 0usize;
    walk_py_files(&out_dir, &mut |path| {
        // Skip the generated typhon_runtime/ helpers — they are infrastructure,
        // not user code, and double-instrumenting them would distort the
        // measurements of the calls into them.
        if path
            .components()
            .any(|c| c.as_os_str() == std::ffi::OsStr::new("typhon_runtime"))
        {
            return Ok(());
        }
        let src = std::fs::read_to_string(path)
            .map_err(|e| miette!("cannot read '{}': {e}", path.display()))?;
        let instrumented_src = instrument_module(&src);
        std::fs::write(path, instrumented_src)
            .map_err(|e| miette!("cannot write '{}': {e}", path.display()))?;
        instrumented += 1;
        Ok(())
    })?;

    // Drop the profiler helper next to the emitted code so the injected
    // imports resolve at runtime without requiring a separate install.
    let helper_path = out_dir.join("typhon_profile.py");
    std::fs::write(&helper_path, TYPHON_PROFILE_PY)
        .map_err(|e| miette!("cannot write '{}': {e}", helper_path.display()))?;

    println!(
        "instrumented {} file(s); profile data will be written to typhon-profile.json on exit",
        instrumented
    );
    Ok(())
}

/// Sampling mode: run the built entry point under `python -m
/// profiling.sampling run --collapsed`, then remap the stacks to `.ty`.
fn run_sampling(args: &ProfileArgs, project_root: &Path, out_dir: &Path) -> Result<()> {
    if let (true, Some(mode)) = (args.async_aware, args.mode.as_deref()) {
        if mode != "wall" {
            return Err(miette!(
                "--async-aware samples wall-clock time only; drop --mode {mode}"
            ));
        }
    }
    let entry = out_dir.join(&args.entry);
    if !entry.exists() {
        return Err(miette!(
            "entry-point '{}' does not exist; pass --entry",
            entry.display()
        ));
    }
    let entry = entry.canonicalize().unwrap_or(entry);
    let interpreter =
        crate::commands::run::project_interpreter(args.python.as_deref(), project_root);

    // Tachyon ships with CPython 3.15. Probe for it so an older interpreter
    // fails with a pointer to `--instrument` rather than a bare
    // `No module named profiling`.
    let probe = Command::new(&interpreter)
        .args(["-c", "import profiling.sampling"])
        .output()
        .map_err(|e| miette!("cannot spawn '{interpreter}': {e}"))?;
    if !probe.status.success() {
        return Err(miette!(
            help = "sampling needs CPython 3.15+; pass --python python3.15, or --instrument for the decorator profiler",
            "'{interpreter}' has no `profiling.sampling` module"
        ));
    }

    let raw_dir = out_dir.join(".profile");
    std::fs::create_dir_all(&raw_dir)
        .map_err(|e| miette!("cannot create '{}': {e}", raw_dir.display()))?;
    let raw_path = raw_dir.join("tachyon.collapsed");
    let _ = std::fs::remove_file(&raw_path);

    let mut cmd = Command::new(&interpreter);
    crate::commands::run::apply_runtime_env(&mut cmd, project_root);
    cmd.args(["-m", "profiling.sampling", "run", "--collapsed", "-o"])
        .arg(&raw_path);
    if let Some(rate) = &args.rate {
        cmd.args(["-r", rate]);
    }
    if let Some(mode) = &args.mode {
        cmd.args(["--mode", mode]);
    }
    if args.async_aware {
        cmd.arg("--async-aware");
    }
    if args.all_threads {
        cmd.arg("-a");
    }
    // Same spawn shape as `tyc run --compile`: module mode when the entry
    // sits inside a package, so its relative imports resolve.
    if let Some((workdir, module_path)) = crate::commands::run::module_invocation(&entry) {
        cmd.current_dir(&workdir).arg("-m").arg(module_path);
    } else {
        cmd.arg(&entry);
    }
    cmd.args(&args.script_args);
    let status = cmd
        .status()
        .map_err(|e| miette!("cannot spawn '{interpreter}': {e}"))?;

    let raw = std::fs::read_to_string(&raw_path).map_err(|e| {
        miette!(
            "the profiler wrote no samples to '{}' ({e}); the program exited with {status}",
            raw_path.display()
        )
    })?;
    let remapped = remap_collapsed(&raw, out_dir, project_root);
    let out_path = project_root.join("typhon-profile.collapsed");
    tyc_format::atomic_write(&out_path, remapped.as_bytes())
        .map_err(|e| miette!("cannot write '{}': {e}", out_path.display()))?;

    print!("{}", summarise(&remapped, args.top));
    println!(
        "wrote {} (collapsed stacks; open in speedscope, inferno or flamegraph.pl)",
        out_path.display()
    );
    if !status.success() {
        return Err(miette!("the profiled program exited with {status}"));
    }
    Ok(())
}

/// Rewrite Tachyon's collapsed stacks so frames from the build point at the
/// `.ty` source. A line is `frame;frame;… COUNT`, and a frame is
/// `FILE:FUNC:LINE` (Tachyon may also emit a leading `tid:N` pseudo-frame,
/// which passes through). Frames whose `.py` has a `.py.map` become
/// `src/rel.ty:FUNC:TY_LINE`, relative to the project root; the
/// interpreter's own leading `<frozen runpy>` frames are dropped. Anything
/// that doesn't parse is kept verbatim.
///
/// Tachyon writes only the file's basename (`os.path.basename`), so a frame
/// is matched against every emitted `.py` under `out_dir` with that name;
/// when two packages share a basename, the one that defines `FUNC` wins,
/// and a frame that stays ambiguous is left unmapped.
fn remap_collapsed(raw: &str, out_dir: &Path, project_root: &Path) -> String {
    let mut by_name: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let _ = walk_py_files(out_dir, &mut |path| {
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            by_name
                .entry(name.to_owned())
                .or_default()
                .push(path.to_path_buf());
        }
        Ok(())
    });
    let mut maps: HashMap<PathBuf, Option<(String, crate::commands::source_map::SourceMap)>> =
        HashMap::new();
    let mut out = String::with_capacity(raw.len());
    for line in raw.lines() {
        let Some((stack, count)) = line.rsplit_once(' ') else {
            if !line.trim().is_empty() {
                out.push_str(line);
                out.push('\n');
            }
            continue;
        };
        let mut frames: Vec<String> = Vec::new();
        for frame in stack.split(';') {
            if frame.starts_with("<frozen runpy>:") {
                continue;
            }
            frames.push(remap_frame(
                frame,
                out_dir,
                project_root,
                &by_name,
                &mut maps,
            ));
        }
        out.push_str(&frames.join(";"));
        out.push(' ');
        out.push_str(count);
        out.push('\n');
    }
    out
}

fn remap_frame(
    frame: &str,
    out_dir: &Path,
    project_root: &Path,
    by_name: &HashMap<String, Vec<PathBuf>>,
    maps: &mut HashMap<PathBuf, Option<(String, crate::commands::source_map::SourceMap)>>,
) -> String {
    use crate::commands::source_map::{load_map_for, map_py_line, parse_map};
    let mut parts = frame.rsplitn(3, ':');
    let (Some(line), Some(func), Some(file)) = (parts.next(), parts.next(), parts.next()) else {
        return frame.to_owned();
    };
    let Ok(py_line) = line.parse::<u32>() else {
        return frame.to_owned();
    };
    if !file.ends_with(".py") {
        return frame.to_owned();
    }
    let py = Path::new(file);
    let py = if py.is_absolute() {
        py.to_path_buf()
    } else if let Some(found) = by_name.get(file).and_then(|c| pick_module(c, func)) {
        found
    } else {
        return frame.to_owned();
    };
    let entry = maps.entry(py.clone()).or_insert_with(|| {
        let py_str = py.to_string_lossy();
        let map = load_map_for(&py_str, Some(out_dir)).and_then(|b| parse_map(&b))?;
        let ty = PathBuf::from(crate::commands::source_map::resolve_ty_path(
            &py_str,
            &map.source,
        ));
        let ty = ty
            .strip_prefix(project_root)
            .map(Path::to_path_buf)
            .unwrap_or(ty);
        Some((ty.to_string_lossy().replace('\\', "/"), map))
    });
    match entry {
        Some((ty, map)) => format!("{ty}:{func}:{}", map_py_line(map, py_line)),
        None => frame.to_owned(),
    }
}

/// The emitted module a basename-only frame came from: the only candidate,
/// or the only one that defines `func` (module-level frames match any).
fn pick_module(candidates: &[PathBuf], func: &str) -> Option<PathBuf> {
    if let [only] = candidates {
        return Some(only.clone());
    }
    if func.starts_with('<') {
        return None;
    }
    let defines = |path: &PathBuf| {
        std::fs::read_to_string(path).is_ok_and(|src| {
            src.lines().any(|l| {
                let l = l.trim_start();
                let l = l.strip_prefix("async ").unwrap_or(l);
                l.strip_prefix("def ")
                    .or_else(|| l.strip_prefix("class "))
                    .is_some_and(|rest| {
                        rest.strip_prefix(func)
                            .is_some_and(|r| r.starts_with(['(', ':', '[']))
                    })
            })
        })
    };
    let mut hits = candidates.iter().filter(|p| defines(p));
    match (hits.next(), hits.next()) {
        (Some(one), None) => Some(one.clone()),
        _ => None,
    }
}

/// A plain-text table of the hottest functions in remapped collapsed stacks:
/// self samples (the function was on top of the stack) and total samples (it
/// was anywhere on the stack, counted once per stack).
fn summarise(collapsed: &str, top: usize) -> String {
    #[derive(Default)]
    struct Row {
        self_n: u64,
        total_n: u64,
        lines: HashMap<String, u64>,
    }
    let mut rows: HashMap<String, Row> = HashMap::new();
    let mut grand = 0u64;
    for line in collapsed.lines() {
        let Some((stack, count)) = line.rsplit_once(' ') else {
            continue;
        };
        let Ok(n) = count.trim().parse::<u64>() else {
            continue;
        };
        grand += n;
        let frames: Vec<&str> = stack
            .split(';')
            .filter(|f| !f.starts_with("tid:") && !f.is_empty())
            .collect();
        let mut seen: Vec<String> = Vec::new();
        for (i, frame) in frames.iter().enumerate() {
            let mut parts = frame.rsplitn(3, ':');
            let (Some(ln), Some(func), Some(file)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let key = format!("{func} ({file})");
            let row = rows.entry(key.clone()).or_default();
            if i + 1 == frames.len() {
                row.self_n += n;
                *row.lines.entry(format!("{file}:{ln}")).or_default() += n;
            }
            if !seen.contains(&key) {
                row.total_n += n;
                seen.push(key);
            }
        }
    }
    let mut sorted: Vec<(String, Row)> = rows.into_iter().collect();
    sorted.sort_by(|a, b| {
        b.1.self_n
            .cmp(&a.1.self_n)
            .then(b.1.total_n.cmp(&a.1.total_n))
            .then(a.0.cmp(&b.0))
    });
    let pct = |n: u64| {
        if grand == 0 {
            0.0
        } else {
            n as f64 * 100.0 / grand as f64
        }
    };
    let mut out = format!(
        "{grand} samples\n{:>7}  {:>7}  function (hottest line)\n",
        "self%", "total%"
    );
    for (key, row) in sorted.into_iter().take(top) {
        let hottest = row
            .lines
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
            .map(|(l, _)| format!(" @ {l}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "{:>6.1}%  {:>6.1}%  {key}{hottest}\n",
            pct(row.self_n),
            pct(row.total_n)
        ));
    }
    out
}

/// Walk every `.py` file under `root`, invoking `f` for each.
fn walk_py_files(
    root: &std::path::Path,
    f: &mut dyn FnMut(&std::path::Path) -> Result<()>,
) -> Result<()> {
    for entry in
        std::fs::read_dir(root).map_err(|e| miette!("cannot list '{}': {e}", root.display()))?
    {
        let entry =
            entry.map_err(|e| miette!("cannot read entry under '{}': {e}", root.display()))?;
        let path = entry.path();
        if path.is_dir() {
            walk_py_files(&path, f)?;
        } else if path.extension().and_then(|s| s.to_str()) == Some("py") {
            f(&path)?;
        }
    }
    Ok(())
}

/// Inject the `typhon_profile` import and decorate every top-level `def` and
/// `async def` in the module body with `@__typhon_profile_record`.  The
/// instrumentation deliberately ignores nested/class methods to keep the
/// transform line-based and predictable; reaching them is a follow-up.
fn instrument_module(source: &str) -> String {
    let header = "import typhon_profile as __typhon_profile_pkg\n\
                  __typhon_profile_record = __typhon_profile_pkg.record\n\
                  __typhon_profile_pkg.ensure_atexit()\n";

    let mut out = String::with_capacity(source.len() + header.len() + 256);

    // Insert the header after any leading `from __future__ import …` lines.
    let mut lines: Vec<&str> = source.split_inclusive('\n').collect();
    let mut insert_at = 0usize;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("from __future__ import")
            || trimmed.is_empty()
            || trimmed.starts_with('#')
        {
            insert_at = i + 1;
            continue;
        }
        break;
    }
    for line in lines.drain(..insert_at) {
        out.push_str(line);
    }
    out.push_str(header);

    // Walk the remaining lines and prepend a decorator before every
    // top-level `def` / `async def`.  Top-level is detected by zero
    // indentation; this matches CPython's notion of module-level functions.
    for line in lines {
        let trimmed_start = line.trim_start();
        if line.len() - trimmed_start.len() == 0
            && (trimmed_start.starts_with("def ") || trimmed_start.starts_with("async def "))
        {
            out.push_str("@__typhon_profile_record\n");
        }
        out.push_str(line);
    }

    out
}

/// Runtime helper dropped into the build directory.  Records call count and
/// cumulative wall-clock time per function and flushes JSON on exit.
const TYPHON_PROFILE_PY: &str = "\
# generated by tyc profile — do not edit
\"\"\"Lightweight profiler injected by `tyc profile`.\"\"\"
from __future__ import annotations

import atexit
import functools
import json
import os
import sys
import time

_REGISTRY: dict[str, dict[str, float]] = {}
_INSTALLED = False


def record(fn):
    \"\"\"Wrap *fn* so each call records (count, total_seconds) in the registry.\"\"\"
    key = f\"{fn.__module__}.{fn.__qualname__}\"

    @functools.wraps(fn)
    def _wrapper(*args, **kwargs):
        start = time.perf_counter()
        try:
            return fn(*args, **kwargs)
        finally:
            elapsed = time.perf_counter() - start
            entry = _REGISTRY.setdefault(key, {\"calls\": 0, \"total_seconds\": 0.0})
            entry[\"calls\"] += 1
            entry[\"total_seconds\"] += elapsed

    return _wrapper


def ensure_atexit() -> None:
    \"\"\"Register the JSON flush hook the first time any module imports us.\"\"\"
    global _INSTALLED
    if _INSTALLED:
        return
    _INSTALLED = True
    atexit.register(_flush)


def _flush() -> None:
    if not _REGISTRY:
        return
    out_path = os.environ.get(\"TYPHON_PROFILE_OUT\", \"typhon-profile.json\")
    try:
        with open(out_path, \"w\", encoding=\"utf-8\") as fh:
            json.dump(_REGISTRY, fh, indent=2, sort_keys=True)
    except OSError as exc:
        print(f\"typhon_profile: cannot write {out_path}: {exc}\", file=sys.stderr)
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remap_collapsed_maps_build_frames_to_ty_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(
            root.join("typhon.toml"),
            "[project]\nname = \"p\"\nsrc = \"src\"\nout = \"build\"\n",
        )
        .unwrap();
        let out = root.join("build");
        std::fs::create_dir_all(out.join(".sourcemaps")).unwrap();
        std::fs::write(out.join("main.py"), "").unwrap();
        std::fs::write(
            out.join(".sourcemaps").join("main.py.map"),
            r#"{"version":2,"source":"main.ty","line_strategy":"table","lines":[1,1,2,7,7,9]}"#,
        )
        .unwrap();
        // Two packages with the same basename: Tachyon writes only `util.py`.
        for (pkg, func) in [("pkg", "parse"), ("other", "render")] {
            std::fs::create_dir_all(out.join(pkg)).unwrap();
            std::fs::create_dir_all(out.join(".sourcemaps").join(pkg)).unwrap();
            std::fs::write(
                out.join(pkg).join("util.py"),
                format!("def {func}(x):\n    return x\n"),
            )
            .unwrap();
            std::fs::write(
                out.join(".sourcemaps").join(pkg).join("util.py.map"),
                format!(
                    r#"{{"version":2,"source":"{pkg}/util.ty","line_strategy":"table","lines":[3,4]}}"#
                ),
            )
            .unwrap();
        }
        let raw = "tid:1;<frozen runpy>:_run_module_as_main:201;<frozen runpy>:_run_code:87;\
                   main.py:<module>:6;main.py:fib:4;decoder.py:decode:345 3\n\
                   tid:1;main.py:<module>:6;main.py:fib:5;util.py:render:2;util.py:<module>:1 2\n";
        let mapped = remap_collapsed(raw, &out, &root);
        assert_eq!(
            mapped,
            "tid:1;src/main.ty:<module>:9;src/main.ty:fib:7;decoder.py:decode:345 3\n\
             tid:1;src/main.ty:<module>:9;src/main.ty:fib:7;src/other/util.ty:render:4;\
             util.py:<module>:1 2\n"
        );
        let table = summarise(&mapped, 10);
        assert!(table.starts_with("5 samples\n"), "{table}");
        assert!(
            table.contains("0.0%   100.0%  fib (src/main.ty)\n"),
            "{table}"
        );
        assert!(
            table.contains("60.0%    60.0%  decode (decoder.py)"),
            "{table}"
        );
    }

    #[test]
    fn remap_collapsed_keeps_unparseable_frames() {
        let tmp = tempfile::tempdir().unwrap();
        let raw = "weird-frame;also:weird 1\nno-count-line\n";
        let mapped = remap_collapsed(raw, tmp.path(), tmp.path());
        assert_eq!(mapped, "weird-frame;also:weird 1\nno-count-line\n");
    }

    #[test]
    fn instrument_module_prepends_header() {
        let src = "def foo():\n    return 1\n";
        let out = instrument_module(src);
        assert!(
            out.contains("import typhon_profile as __typhon_profile_pkg"),
            "header import must be injected; got:\n{out}"
        );
        assert!(
            out.contains("__typhon_profile_pkg.ensure_atexit()"),
            "ensure_atexit call must be injected; got:\n{out}"
        );
    }

    #[test]
    fn instrument_module_decorates_top_level_def() {
        let src = "def foo():\n    return 1\n";
        let out = instrument_module(src);
        assert!(
            out.contains("@__typhon_profile_record\ndef foo("),
            "top-level def must be decorated; got:\n{out}"
        );
    }

    #[test]
    fn instrument_module_decorates_async_def() {
        let src = "async def foo():\n    return 1\n";
        let out = instrument_module(src);
        assert!(
            out.contains("@__typhon_profile_record\nasync def foo("),
            "async def must be decorated; got:\n{out}"
        );
    }

    #[test]
    fn instrument_module_skips_nested_def() {
        // Methods inside a class body are not at indent 0; v1 skips them.
        let src = "class Foo:\n    def bar(self):\n        return 1\n";
        let out = instrument_module(src);
        assert!(
            !out.contains("@__typhon_profile_record\n    def bar"),
            "nested def must NOT be decorated in v1; got:\n{out}"
        );
    }

    #[test]
    fn instrument_module_preserves_future_imports_at_top() {
        let src = "from __future__ import annotations\ndef foo():\n    return 1\n";
        let out = instrument_module(src);
        let future_pos = out.find("from __future__").unwrap();
        let header_pos = out.find("import typhon_profile").unwrap();
        assert!(
            future_pos < header_pos,
            "__future__ must precede the injected header; got:\n{out}"
        );
    }
}
