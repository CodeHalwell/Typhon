//! `tyc profile` — instrument emitted Python code for hot-function detection.
//!
//! Runs the standard `tyc build` pipeline, then post-processes each emitted
//! `.py` file to wrap every top-level function in a lightweight timing
//! decorator that accumulates wall-clock samples into a process-wide
//! registry.  The registry is flushed to `typhon-profile.json` on
//! interpreter shutdown via an `atexit` hook injected into each module.
//!
//! Use this output to find the hottest functions before promoting them with
//! `@pure(memo=True)` or `@memo`, or before reaching for PGO.  v1 reports
//! call counts and total time only; per-call latency histograms are a
//! follow-up.

use std::path::PathBuf;

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
}

pub fn run(args: ProfileArgs) -> Result<()> {
    // First produce a normal build; the profiler post-processes the output
    // rather than re-implementing the pipeline.
    build::run(build::BuildArgs {
        path: args.path.clone(),
        out: args.out.clone(),
        no_format: false,
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
                project_root.join(out)
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

/// Walk every `.py` file under `root`, invoking `f` for each.
fn walk_py_files(
    root: &std::path::Path,
    f: &mut dyn FnMut(&std::path::Path) -> Result<()>,
) -> Result<()> {
    for entry in std::fs::read_dir(root)
        .map_err(|e| miette!("cannot list '{}': {e}", root.display()))?
    {
        let entry = entry.map_err(|e| miette!("cannot read entry under '{}': {e}", root.display()))?;
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
        assert!(future_pos < header_pos, "__future__ must precede the injected header; got:\n{out}");
    }
}
