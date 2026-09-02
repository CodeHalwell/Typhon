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

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use clap::Args;
use miette::{miette, Result};
use notify::{Event, EventKind, RecursiveMode, Watcher};

use crate::commands::build::{self, BuildArgs};
use crate::commands::source_map::{
    load_map_for, map_py_line, parse_map, resolve_ty_path, SourceMap,
};
use crate::config::TyphonConfig;

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

    /// Watch the project source directory and re-run `tyc ty` whenever a
    /// `.ty` or `.dty` file changes. The initial build + check runs
    /// immediately; subsequent runs are debounced so a burst of editor
    /// "save" events triggers a single re-run. Press Ctrl+C to stop.
    #[arg(long)]
    pub watch: bool,

    /// Disable diagnostic attribution. By default `tyc ty` captures
    /// `ty`'s stdout/stderr and rewrites `path.py:line[:col]` prefixes
    /// to the originating `.ty` source via the `.py.map` sidecars
    /// emitted by `tyc build`. Pass `--raw` to forward `ty`'s output
    /// verbatim (useful when piping into other tools that expect the
    /// Python file paths, or when debugging the map itself).
    #[arg(long)]
    pub raw: bool,
}

pub fn run(args: TyArgs) -> Result<()> {
    if args.no_build && args.out.is_none() {
        return Err(miette!(
            "--no-build requires --out so the output directory is known"
        ));
    }
    if args.watch && args.no_build {
        return Err(miette!(
            "--watch cannot be combined with --no-build (nothing to rebuild on change)"
        ));
    }

    if args.watch {
        return run_watch(args);
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
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
            source_label: None,
        })?;
    }

    run_ty_check(&args.path, &out_dir, &args.ty_bin, args.raw, &args.ty_args)
}

/// Run `ty check <out_dir>` anchored at `project_dir`, remap its
/// `path.py:line[:col]` diagnostics back to the originating `.ty` source via
/// the `.py.map` sidecars in `out_dir`, and print them. Returns `Err` when
/// the `ty` binary is missing or `ty` reported type errors.
///
/// Shared by the `tyc ty` command and the `[checker] external = "ty"` /
/// `--with-ty` hook on `tyc build` and `tyc check`. Anchoring the subprocess
/// at the project root lets `ty` discover the project's `pyproject.toml` /
/// virtualenv (so installed third-party packages and their typeshed stubs
/// resolve); the output directory is passed absolute so it isn't
/// double-anchored.
pub fn run_ty_check(
    project_dir: &Path,
    out_dir: &Path,
    ty_bin: &str,
    raw: bool,
    extra_args: &[String],
) -> Result<()> {
    let mut cmd = Command::new(ty_bin);
    cmd.current_dir(project_dir);
    cmd.arg("check").arg(out_dir);
    for extra in extra_args {
        cmd.arg(extra);
    }

    if raw {
        // Forward verbatim — no capture, no remapping.
        let status = cmd.status().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => missing_ty_binary_error(ty_bin),
            _ => miette!("failed to run `{} check`: {e}", ty_bin),
        })?;
        if !status.success() {
            return Err(miette!(
                "`{}` reported type errors (exit {})",
                ty_bin,
                status.code().unwrap_or(-1)
            ));
        }
        return Ok(());
    }

    // Capture stdout + stderr so `path.py:line[:col]` references can be
    // rewritten to the originating Typhon source via the `.py.map`
    // sidecars next to each emitted `.py`.
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let output = cmd.output().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => missing_ty_binary_error(ty_bin),
        _ => miette!("failed to run `{} check`: {e}", ty_bin),
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mapped_stdout = remap_ty_diagnostics(&stdout, Some(out_dir));
    let mapped_stderr = remap_ty_diagnostics(&stderr, Some(out_dir));
    print!("{mapped_stdout}");
    eprint!("{mapped_stderr}");

    if !output.status.success() {
        return Err(miette!(
            "`{}` reported type errors (exit {})",
            ty_bin,
            output.status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

fn missing_ty_binary_error(bin: &str) -> miette::Report {
    miette!(
        "`{}` not found on PATH — install Astral's `ty` (e.g. `pip install ty` \
         or `uv tool install ty`) or pass --ty-bin to point at your install",
        bin,
    )
}

/// Debounce window: a burst of filesystem events (a typical editor save
/// touches several files in quick succession) are coalesced into a single
/// rebuild so we don't kick off N redundant ty checks.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(300);

/// Resolve the source directory the watcher should observe. Mirrors the
/// resolution `tyc build` performs: look up `typhon.toml` from
/// `project_path` (or any ancestor), then join its `[project] src`
/// against the directory containing that toml. Falls back to
/// `project_path/src` (the default `src = "src"`) when no config file
/// is found, which matches `tyc build`'s fallback behaviour.
fn resolve_watched_src_dir(project_path: &Path) -> Result<PathBuf> {
    match TyphonConfig::load(project_path) {
        Ok(Some((toml_path, cfg))) => {
            let dir = toml_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| project_path.to_path_buf());
            Ok(dir.join(&cfg.project.src))
        }
        Ok(None) => Ok(project_path.join("src")),
        Err(e) => Err(miette!("failed to read typhon.toml: {e}")),
    }
}

/// Run-once-then-watch loop for `tyc ty --watch`.
///
/// Watches the project's source directory (resolved via `typhon.toml`'s
/// `[project] src = …`, matching `tyc build`) recursively for `.ty` /
/// `.dty` changes and re-runs the full `tyc ty` pipeline each time.
/// Errors from any single iteration are printed but don't tear down the
/// watcher — the user fixes their code and the next save triggers
/// another run.
fn run_watch(args: TyArgs) -> Result<()> {
    let src_dir = resolve_watched_src_dir(&args.path)?;
    if !src_dir.exists() {
        return Err(miette!(
            "watch target '{}' does not exist; check the `[project] src` setting in typhon.toml",
            src_dir.display()
        ));
    }

    // Channel-backed watcher: the OS-specific backend posts events here,
    // the main thread debounces and re-runs.
    let (tx, rx) = channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        if let Ok(event) = res {
            let _ = tx.send(event);
        }
    })
    .map_err(|e| miette!("failed to install file watcher: {e}"))?;
    watcher
        .watch(&src_dir, RecursiveMode::Recursive)
        .map_err(|e| miette!("failed to watch '{}': {e}", src_dir.display()))?;

    eprintln!(
        "tyc ty --watch: watching {} (Ctrl+C to stop)",
        src_dir.display()
    );

    // Initial run so the user sees the current state immediately.
    if let Err(e) = run_once(&args) {
        eprintln!("error: {:?}", e);
    }

    while let Ok(first) = rx.recv() {
        // Debounce by draining further events that arrive within
        // WATCH_DEBOUNCE of the most recent one. A typical editor save
        // emits Create + Modify + Remove in rapid succession, and
        // unrelated events (vim swap files, formatter touches, …) may
        // be interleaved. We re-run whenever *any* event in the
        // debounce window touched a `.ty` / `.dty` file — never bail
        // out early on a lone irrelevant event, otherwise an editor
        // that emits its temp-file event first would mask the real
        // source-file modify event behind it.
        let mut relevant = event_is_relevant(&first);
        let mut deadline = Instant::now() + WATCH_DEBOUNCE;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(remaining) {
                Ok(ev) => {
                    if event_is_relevant(&ev) {
                        relevant = true;
                    }
                    // Extend the debounce window from the latest event so
                    // a steady stream of saves coalesces into one rebuild.
                    deadline = Instant::now() + WATCH_DEBOUNCE;
                }
                Err(_) => break,
            }
        }

        if !relevant {
            continue;
        }

        eprintln!("tyc ty --watch: change detected, re-running…");
        if let Err(e) = run_once(&args) {
            eprintln!("error: {:?}", e);
        }
    }
    Ok(())
}

/// One pass of the build + `ty check` pipeline. Cloned-arg variant of the
/// non-watch path so the watch loop can call it repeatedly.
fn run_once(args: &TyArgs) -> Result<()> {
    let cloned = TyArgs {
        path: args.path.clone(),
        out: args.out.clone(),
        ty_bin: args.ty_bin.clone(),
        no_build: false,
        ty_args: args.ty_args.clone(),
        watch: false,
        raw: false,
    };
    run(cloned)
}

/// `true` when a filesystem event touches a Typhon source file. We
/// deliberately ignore changes to the output directory (otherwise the
/// build itself would trigger an immediate re-build loop) and any
/// non-source extensions.
fn event_is_relevant(ev: &Event) -> bool {
    if !matches!(
        ev.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    ) {
        return false;
    }
    ev.paths.iter().any(|p| has_typhon_extension(p))
}

fn has_typhon_extension(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|s| s.to_str()),
        Some("ty") | Some("dty")
    )
}

// ── ty diagnostic remapping ───────────────────────────────────────────────────

/// Rewrite every `path.py:LINE[:COL]:` prefix in `text` to point at the
/// originating `.ty` source via the adjacent `.py.map` sidecar.
///
/// `ty` (and most Python tooling) prints diagnostics as
/// `path.py:line:col: severity: message`. We scan each output line for a
/// leading `*.py:NN(:NN)?` reference, load the matching map, and emit
/// `path.ty:LINE[:COL]:` with the table-mapped line. Lines without a
/// recognisable `.py` reference are forwarded verbatim, so summary text,
/// blank lines, and unrelated tool output round-trip unchanged.
pub fn remap_ty_diagnostics(text: &str, map_dir: Option<&Path>) -> String {
    use std::collections::HashMap;

    let mut out = String::with_capacity(text.len());
    // Cache of parsed maps keyed by `.py` path. `ty`'s default
    // `--output-format full` can emit many lines for a single
    // diagnostic (header, `-->` location, snippet, secondary
    // notes), and each one can reference the same `.py` file.
    // Loading + parsing the sidecar once per file across the whole
    // output keeps the cost O(unique-files) instead of O(lines).
    // `None` is cached so a missing or malformed sidecar isn't
    // re-discovered on every subsequent line that references the
    // same file.
    let mut cache: HashMap<String, Option<SourceMap>> = HashMap::new();
    for line in text.split_inclusive('\n') {
        out.push_str(&remap_one_diagnostic_line(line, map_dir, &mut cache));
    }
    out
}

/// Rewrite one output line if it carries a `path.py:line[:col]` prefix.
///
/// Handles both `--output-format concise` and the default
/// `--output-format full` (Rust-like multi-line) — only the line that
/// contains the path:line:col span needs rewriting; surrounding context
/// lines are forwarded verbatim because the `ty` output already paints
/// snippet excerpts from the Python file, which would be confusing to
/// rewrite mid-stream. Users who want full attribution should fix the
/// Typhon source and re-run.
fn remap_one_diagnostic_line(
    line: &str,
    map_dir: Option<&Path>,
    cache: &mut std::collections::HashMap<String, Option<SourceMap>>,
) -> String {
    // Cheap reject: the line must contain ".py:" to be a candidate.
    if !line.contains(".py:") {
        return line.to_owned();
    }
    let Some(parsed) = parse_py_ref_with_validator(line, map_dir) else {
        return line.to_owned();
    };

    let map = match cache.get(&parsed.py_path) {
        Some(slot) => slot.as_ref(),
        None => {
            let parsed_map = load_map_for(&parsed.py_path, map_dir)
                .as_deref()
                .and_then(parse_map);
            cache.insert(parsed.py_path.clone(), parsed_map);
            cache.get(&parsed.py_path).and_then(|s| s.as_ref())
        }
    };
    let Some(map) = map else {
        return line.to_owned();
    };

    let ty_line = map_py_line(map, parsed.line);
    let ty_path = resolve_ty_path(&parsed.py_path, &map.source);

    let mut new_ref = format!("{ty_path}:{ty_line}");
    if let Some(col) = parsed.col {
        new_ref.push(':');
        new_ref.push_str(&col.to_string());
    }

    // Splice replacement into the original line so suffix formatting
    // (severity tag, message, ANSI codes, etc.) is preserved.
    let before = &line[..parsed.start];
    let after = &line[parsed.end..];
    format!("{before}{new_ref}{after}")
}

#[derive(Debug, PartialEq)]
struct PyRef {
    py_path: String,
    line: u32,
    col: Option<u32>,
    /// Byte offset of the full `path.py:line[:col]` substring in the
    /// containing line.
    start: usize,
    end: usize,
}

/// Locate a `path.py:LINE(:COL)?` reference in `line`.
///
/// The path can include directory separators and spaces. When a `.py:`
/// pattern is found, the parser walks backward to determine the longest
/// valid path prefix by checking against the filesystem + `.py.map` lookup.
/// Returns `None` when no candidate is present.
///
/// This is a convenience wrapper for tests; production code should use
/// `parse_py_ref_with_validator` with a map_dir for space support.
#[cfg(test)]
fn parse_py_ref(line: &str) -> Option<PyRef> {
    parse_py_ref_with_validator(line, None)
}

/// Internal implementation that accepts an optional map_dir for validation.
/// When `map_dir` is provided, the parser validates candidate paths against
/// the filesystem to support paths with spaces.
fn parse_py_ref_with_validator(line: &str, map_dir: Option<&Path>) -> Option<PyRef> {
    // Helper to check if a .py.map file exists without loading it
    fn map_exists_for(py_path: &str, map_dir: Option<&Path>) -> bool {
        let adjacent = PathBuf::from(format!("{py_path}.map"));
        if adjacent.exists() {
            return true;
        }
        if let Some(dir) = map_dir {
            // `.sourcemaps/<rel>.py.map` for build-relative refs (the embedded
            // `ty` renderer emits these), then the legacy `<dir>/<base>.py.map`.
            if Path::new(py_path).is_relative()
                && dir
                    .join(".sourcemaps")
                    .join(format!("{py_path}.map"))
                    .exists()
            {
                return true;
            }
            if let Some(base) = Path::new(py_path).file_name() {
                let candidate = dir.join(format!("{}.map", base.to_string_lossy()));
                if candidate.exists() {
                    return true;
                }
            }
        }
        false
    }

    // Find every `.py:` occurrence; the path is everything to the left.
    let bytes = line.as_bytes();
    let mut search_from = 0;
    while let Some(rel) = line[search_from..].find(".py:") {
        let py_end = search_from + rel + 3; // position of `:` after `.py`

        // Strategy for paths with spaces:
        // 1. Find the start using the old heuristic (stop at delimiters)
        // 2. If map_dir is provided, try longer prefixes and validate against filesystem
        // 3. Use the longest valid match

        // Old heuristic: walk left to find start of path token
        let mut start = py_end - 3; // start at the `.` in `.py`
        while start > 0 {
            let b = bytes[start - 1];
            // Stop at whitespace, quotes, opening punctuation, or a newline.
            if matches!(
                b,
                b' ' | b'\t' | b'\n' | b'\r' | b'\'' | b'"' | b'(' | b'[' | b'<' | b'>'
            ) {
                break;
            }
            start -= 1;
        }

        // Trim any leading spaces from the start position (common in formats like "  --> path.py:line")
        while start < py_end && bytes[start] == b' ' {
            start += 1;
        }

        // Now we have a conservative start position. If we have a validator,
        // try extending backward through spaces by validating against the filesystem.
        let py_path = if let Some(map_dir) = map_dir {
            // Try successively longer candidate prefixes
            let mut best_candidate = &line[start..py_end - 3 + 3]; // up to and incl. `.py`
            let mut best_start = start;

            // Walk further back, but only through spaces and valid path characters
            let mut test_start = start;
            while test_start > 0 {
                let b = bytes[test_start - 1];
                // Only extend through alphanumeric, path separators, and spaces
                if !matches!(b, b' ' | b'/' | b'\\' | b'.' | b'-' | b'_')
                    && !b.is_ascii_alphanumeric()
                {
                    break;
                }
                test_start -= 1;

                let candidate = &line[test_start..py_end - 3 + 3];
                // Trim leading spaces from candidate before validation
                let trimmed_candidate = candidate.trim_start();
                // Validate: does this candidate have a .py.map file?
                if map_exists_for(trimmed_candidate, Some(map_dir)) {
                    // Update start to exclude the leading spaces we just trimmed
                    let trim_offset = candidate.len() - trimmed_candidate.len();
                    best_candidate = trimmed_candidate;
                    best_start = test_start + trim_offset;
                }
            }

            start = best_start;
            best_candidate
        } else {
            &line[start..py_end - 3 + 3]
        };

        // After the `:` parse the line number.
        let after = &line[py_end + 1..];
        let digit_len = after.bytes().take_while(|b| b.is_ascii_digit()).count();
        if digit_len == 0 {
            search_from = py_end + 1;
            continue;
        }
        let line_num: u32 = after[..digit_len].parse().ok()?;
        // Optional `:col` suffix.
        let after_line = &after[digit_len..];
        let (col, col_consumed) = if let Some(rest) = after_line.strip_prefix(':') {
            let col_digits = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
            if col_digits > 0 {
                let col_val: Option<u32> = rest[..col_digits].parse().ok();
                (col_val, 1 + col_digits)
            } else {
                (None, 0)
            }
        } else {
            (None, 0)
        };

        let end = py_end + 1 + digit_len + col_consumed;
        return Some(PyRef {
            py_path: py_path.to_owned(),
            line: line_num,
            col,
            start,
            end,
        });
    }
    None
}

#[cfg(test)]
mod ty_remap_tests {
    use super::*;
    use std::io::Write;

    /// Plant a `.py` + `.py.map` pair in a tempdir and assert the
    /// reference is rewritten.
    fn with_planted_map<R>(
        py_rel: &str,
        ty_source: &str,
        lines: &[u32],
        f: impl FnOnce(&Path) -> R,
    ) -> R {
        let tmp = tempfile::tempdir().unwrap();
        let py_path = tmp.path().join(py_rel);
        std::fs::create_dir_all(py_path.parent().unwrap()).unwrap();
        let mut f_py = std::fs::File::create(&py_path).unwrap();
        writeln!(f_py, "# emitted").unwrap();
        let map_path = tmp.path().join(format!("{py_rel}.map"));
        let lines_json = serde_json::to_string(lines).unwrap();
        std::fs::write(
            &map_path,
            format!(
                "{{\"version\":2,\"source\":\"{ty_source}\",\"line_strategy\":\"table\",\"lines\":{lines_json}}}"
            ),
        )
        .unwrap();
        f(tmp.path())
    }

    #[test]
    fn parse_py_ref_simple() {
        let r = parse_py_ref("foo.py:42:3: error: x").unwrap();
        assert_eq!(r.py_path, "foo.py");
        assert_eq!(r.line, 42);
        assert_eq!(r.col, Some(3));
    }

    #[test]
    fn parse_py_ref_no_col() {
        let r = parse_py_ref("a/b.py:7: warning: y").unwrap();
        assert_eq!(r.py_path, "a/b.py");
        assert_eq!(r.line, 7);
        assert_eq!(r.col, None);
    }

    #[test]
    fn parse_py_ref_with_arrow_prefix() {
        // `ty`'s `--output-format full` style: `  --> path.py:LINE:COL`.
        let r = parse_py_ref("  --> src/main.py:12:5").unwrap();
        assert_eq!(r.py_path, "src/main.py");
        assert_eq!(r.line, 12);
        assert_eq!(r.col, Some(5));
    }

    #[test]
    fn parse_py_ref_no_match() {
        assert!(parse_py_ref("just some prose without a file ref").is_none());
        // No digits after `.py:`.
        assert!(parse_py_ref("a.py:not-a-line").is_none());
    }

    #[test]
    fn remap_one_line_rewrites_path_and_line() {
        with_planted_map("main.py", "main.ty", &[1, 1, 2, 3, 3], |dir| {
            let py = dir.join("main.py").to_string_lossy().into_owned();
            let input = format!("{py}:3:1: error: type mismatch");
            let mut cache = std::collections::HashMap::new();
            let out = remap_one_diagnostic_line(&input, None, &mut cache);
            // Line 3 → ty 2 per the table; path resolves to "main.ty"
            // (no typhon.toml planted, so the fallback returns the
            // map's `source` field verbatim).
            assert!(out.contains("main.ty:2:1"), "got: {out}");
            assert!(out.ends_with("error: type mismatch"));
        });
    }

    #[test]
    fn remap_diagnostics_preserves_unrelated_lines() {
        let text = "Hello world\nFound 0 errors\n";
        let out = remap_ty_diagnostics(text, None);
        assert_eq!(out, text);
    }

    #[test]
    fn remap_diagnostics_skips_when_no_map_exists() {
        // A .py reference but no sidecar: forward verbatim.
        let text = "ghost.py:5:1: error: x\n";
        let out = remap_ty_diagnostics(text, None);
        assert_eq!(out, text);
    }

    #[test]
    fn remap_diagnostics_handles_multi_line_per_file_input() {
        // `ty --output-format full` emits several lines per
        // diagnostic, many of which reference the same `.py:line:col`.
        // The per-file map cache means each file is loaded + parsed
        // exactly once — we exercise the path with a multi-line
        // input here so a regression that re-parses per line would
        // still produce the right output but visibly stress the
        // hot path.
        with_planted_map("multi.py", "multi.ty", &[1, 1, 2, 3, 3, 4], |dir| {
            let py = dir.join("multi.py").to_string_lossy().into_owned();
            let input = format!("{py}:3:1: error: foo\n  --> {py}:3:1\n{py}:5:2: warning: bar\n");
            let out = remap_ty_diagnostics(&input, None);
            // Every `.py:line[:col]` got rewritten via the table.
            assert!(
                out.contains("multi.ty:2:1"),
                "first line rewrite missing: {out}"
            );
            assert!(
                out.contains("multi.ty:3:2"),
                "third line rewrite missing: {out}"
            );
            assert!(!out.contains("multi.py"), "raw .py refs leaked: {out}");
        });
    }
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
            watch: false,
            raw: false,
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
            watch: false,
            raw: false,
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
            watch: false,
            raw: false,
        });
        assert!(result.is_err(), "missing binary should error");
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("not found on PATH") && msg.contains("pip install ty"),
            "error should hint at install: {msg}",
        );
    }

    #[test]
    fn watch_with_no_build_errors() {
        // `--watch` is incompatible with `--no-build`: the whole point of
        // watching is to rebuild on change.
        let result = run(TyArgs {
            path: PathBuf::from("."),
            out: Some(PathBuf::from("build")),
            ty_bin: "ty".into(),
            no_build: true,
            ty_args: vec![],
            watch: true,
            raw: false,
        });
        assert!(result.is_err());
        let msg = format!("{:?}", result.unwrap_err());
        assert!(msg.contains("--watch cannot be combined with --no-build"));
    }

    #[test]
    fn watch_without_src_dir_errors() {
        let tmp = tempfile::tempdir().unwrap();
        // Don't create the source directory — watch should reject this
        // with a useful message that points at the config setting.
        let result = run(TyArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            ty_bin: "ty".into(),
            no_build: false,
            ty_args: vec![],
            watch: true,
            raw: false,
        });
        assert!(result.is_err());
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("does not exist") && msg.contains("typhon.toml"),
            "error should mention the missing dir + the config knob: {msg}"
        );
    }

    #[test]
    fn watch_honours_custom_src_dir_in_typhon_toml() {
        // A project that configures `[project] src = "source"` should be
        // watched at `<project>/source`, not the default `<project>/src`.
        let tmp = tempfile::tempdir().unwrap();
        let custom = tmp.path().join("source");
        std::fs::create_dir_all(&custom).unwrap();
        std::fs::write(custom.join("main.ty"), "let x: int = 1\n").unwrap();
        std::fs::write(
            tmp.path().join("typhon.toml"),
            "[project]\nname = \"t\"\nsrc = \"source\"\n",
        )
        .unwrap();
        let resolved = resolve_watched_src_dir(tmp.path()).unwrap();
        assert_eq!(resolved, custom);
    }

    #[test]
    fn watch_uses_default_src_when_typhon_toml_omits_src() {
        // A typhon.toml that omits `[project] src` falls back to the
        // default `src = "src"` — the same observable behaviour as a
        // project with no toml at all.  We plant the toml inside the
        // tempdir (rather than testing the truly "no toml anywhere"
        // branch) so `TyphonConfig::load`'s ancestor walk terminates
        // here, instead of escaping upward and latching onto a stray
        // `/tmp/typhon.toml` the host filesystem might carry — which
        // would otherwise resolve to `/tmp/src` and fail the assertion.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("typhon.toml"), "[project]\nname = \"t\"\n").unwrap();
        let resolved = resolve_watched_src_dir(tmp.path()).unwrap();
        assert_eq!(resolved, tmp.path().join("src"));
    }

    #[test]
    fn has_typhon_extension_recognises_ty_and_dty() {
        assert!(has_typhon_extension(Path::new("a.ty")));
        assert!(has_typhon_extension(Path::new("a.dty")));
        assert!(!has_typhon_extension(Path::new("a.py")));
        assert!(!has_typhon_extension(Path::new("a")));
    }
}
