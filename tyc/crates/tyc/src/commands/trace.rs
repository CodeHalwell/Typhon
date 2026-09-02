//! `tyc trace` — map a Python traceback back to Typhon source.
//!
//! Reads a Python traceback (from a file or stdin) and rewrites every
//! `  File "path/to/foo.py", line N, in func` frame that has an adjacent
//! `.py.map` sidecar. The sidecar encodes the corresponding `.ty` source
//! path and, in v2 maps, a per-line mapping table for sugar-expanded
//! constructs (`?`, `gather:`, `with`-chains).

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use clap::Args;
use miette::{miette, Result};

use crate::commands::source_map::{
    load_map_for, map_py_line, parse_map, resolve_ty_path, SourceMap,
};

/// Arguments for `tyc trace`.
#[derive(Args, Debug)]
pub struct TraceArgs {
    /// Python traceback file to map (reads from stdin if omitted).
    #[arg(value_name = "TRACEBACK")]
    pub traceback: Option<PathBuf>,

    /// Directory containing `.py.map` source-map files.
    #[arg(long, value_name = "DIR")]
    pub map_dir: Option<PathBuf>,
}

pub fn run(args: TraceArgs) -> Result<()> {
    let text = match &args.traceback {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|e| miette!("cannot read '{}': {e}", path.display()))?,
        None => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| miette!("cannot read stdin: {e}"))?;
            buf
        }
    };

    let rewritten = rewrite_traceback(&text, args.map_dir.as_deref());
    print!("{rewritten}");
    Ok(())
}

// ── core rewrite ─────────────────────────────────────────────────────────────

/// Rewrite every Python `  File "PATH.py", line N` frame in `text`.
///
/// The rows *under* a frame are rewritten too. CPython prints the emitted
/// `.py` source there, plus a row of column anchors; leaving them under a
/// `.ty` path and line is worse than showing nothing, because the reader
/// opens that line and finds something else — and a generated name
/// (`__typhon_qi_0__`) leaks into the very traceback the source map exists
/// to keep clean. The `.ty` row is substituted in and the anchors, whose
/// columns no longer mean anything, are dropped.
pub fn rewrite_traceback(text: &str, map_dir: Option<&Path>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut sources: HashMap<String, Option<Vec<String>>> = HashMap::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let rewritten = try_rewrite_frame(line, map_dir);
        let remapped = rewritten != line;
        out.push_str(&rewritten);
        out.push('\n');
        i += 1;
        if !remapped {
            continue;
        }
        // A source row under the frame: indented, and not a frame itself.
        if i < lines.len()
            && lines[i].starts_with(' ')
            && !lines[i].trim_start().starts_with("File \"")
        {
            let py_dir = frame_file(line)
                .and_then(|p| {
                    Path::new(&p)
                        .parent()
                        .map(|d| d.to_string_lossy().into_owned())
                })
                .unwrap_or_default();
            match ty_source_row(&rewritten, &py_dir, &mut sources) {
                Some(text) => {
                    out.push_str("    ");
                    out.push_str(&text);
                    out.push('\n');
                    i += 1;
                    // The anchors point into the row we just replaced.
                    if i < lines.len() && is_anchor_row(lines[i]) {
                        i += 1;
                    }
                }
                // The `.ty` file is not readable from here (a traceback
                // pasted on another machine, say). Keep what CPython
                // printed rather than dropping the row.
                None => {
                    out.push_str(lines[i]);
                    out.push('\n');
                    i += 1;
                }
            }
        }
    }
    // Preserve trailing-newline parity with the original.
    if !text.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    out
}

/// The path inside a `File "PATH", line N` frame row.
fn frame_file(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix("File \"")?;
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

/// A 3.11+ column-anchor row (`    ~~~~^^^^`) under a source row.
fn is_anchor_row(line: &str) -> bool {
    let body = line.trim();
    !body.is_empty() && body.chars().all(|c| c == '~' || c == '^')
}

/// The `.ty` source row a rewritten frame header points at, if it can be
/// read. `sources` caches one read per file (and its failure).
fn ty_source_row(
    frame: &str,
    py_dir: &str,
    sources: &mut HashMap<String, Option<Vec<String>>>,
) -> Option<String> {
    let rest = frame.trim_start().strip_prefix("File \"")?;
    let path_end = rest.find('"')?;
    let path = &rest[..path_end];
    let (line_no, _, _) = parse_line_suffix(&rest[path_end + 1..])?;
    // `resolve_ty_path` falls back to the bare source name when there is no
    // `typhon.toml` next to the build. Read that relative to the emitted
    // file's own directory rather than the process's working directory, so
    // an unrelated file of the same name is never picked up.
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        Path::new(py_dir).join(path)
    };
    let lines = sources.entry(path.to_owned()).or_insert_with(|| {
        std::fs::read_to_string(&candidate)
            .ok()
            .map(|t| t.lines().map(str::to_owned).collect())
    });
    let lines = lines.as_ref()?;
    let row = lines.get(line_no.checked_sub(1)? as usize)?.trim();
    (!row.is_empty()).then(|| row.to_owned())
}

/// Try to rewrite one frame line; return it unchanged when no rewrite applies.
fn try_rewrite_frame(line: &str, map_dir: Option<&Path>) -> String {
    let trimmed = line.trim_start();
    let leading_spaces = line.len() - trimmed.len();

    let Some(rest) = trimmed.strip_prefix("File \"") else {
        return line.to_owned();
    };

    // Find the closing quote of the file path.
    let Some(path_end) = rest.find('"') else {
        return line.to_owned();
    };
    let py_path = &rest[..path_end];

    // Only rewrite Python source files; leave C extensions etc. alone.
    if !py_path.ends_with(".py") {
        return line.to_owned();
    }

    // Everything after the closing quote: `, line N, in func`.
    let after_path = &rest[path_end + 1..];

    let map_body = load_map_for(py_path, map_dir);
    let Some(map) = map_body.as_deref().and_then(parse_map) else {
        return line.to_owned();
    };

    let ty_path = resolve_ty_path(py_path, &map.source);
    let new_after = apply_line_map(&map, after_path);

    let indent = " ".repeat(leading_spaces);
    format!("{indent}File \"{ty_path}\"{new_after}")
}

// ── line mapping ──────────────────────────────────────────────────────────────

/// Rewrite the `", line N, in func"` suffix using the map's line strategy.
fn apply_line_map(map: &SourceMap, after_path: &str) -> String {
    let Some((py_line, num_offset, digit_len)) = parse_line_suffix(after_path) else {
        return after_path.to_owned();
    };

    let ty_line = map_py_line(map, py_line);

    if ty_line == py_line {
        return after_path.to_owned();
    }

    let before_num = &after_path[..num_offset];
    let after_num = &after_path[num_offset + digit_len..];
    format!("{before_num}{ty_line}{after_num}")
}

/// Parse the line number from `, line N[, in func]` and return
/// `(line_number, byte_offset_of_number_start, digit_byte_length)`.
fn parse_line_suffix(s: &str) -> Option<(u32, usize, usize)> {
    // s = `, line 42, in func`
    let s_stripped = s.strip_prefix(',')?;
    let s_stripped = s_stripped.trim_start();
    let s_stripped = s_stripped.strip_prefix("line ")?;
    // s_stripped now starts with the number.
    let num_offset = s.len() - s_stripped.len();
    let digit_end = s_stripped
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(s_stripped.len());
    let num: u32 = s_stripped[..digit_end].parse().ok()?;
    Some((num, num_offset, digit_end))
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::source_map::LineStrategy;

    // ── map parsing ──────────────────────────────────────────────────────────

    #[test]
    fn parse_map_v1_identity() {
        let body = r#"{"version":1,"source":"main.ty","line_strategy":"identity"}"#;
        let map = parse_map(body).expect("should parse");
        assert_eq!(map.source, "main.ty");
        assert_eq!(map.strategy, LineStrategy::Identity);
        assert!(map.lines.is_empty());
    }

    #[test]
    fn parse_map_v2_table() {
        let body =
            r#"{"version":2,"source":"sub/main.ty","line_strategy":"table","lines":[1,1,2,3,3,4]}"#;
        let map = parse_map(body).expect("should parse");
        assert_eq!(map.source, "sub/main.ty");
        assert_eq!(map.strategy, LineStrategy::Table);
        assert_eq!(map.lines, vec![1, 1, 2, 3, 3, 4]);
    }

    #[test]
    fn parse_map_with_path_escapes() {
        let body = r#"{"version":1,"source":"sub\\main.ty","line_strategy":"identity"}"#;
        let map = parse_map(body).expect("should parse");
        assert_eq!(map.source, "sub\\main.ty");
    }

    #[test]
    fn parse_map_missing_source_returns_none() {
        let body = r#"{"version":1,"line_strategy":"identity"}"#;
        assert!(parse_map(body).is_none());
    }

    // ── line suffix parsing ──────────────────────────────────────────────────

    #[test]
    fn parse_line_suffix_basic() {
        let (num, _, _) = parse_line_suffix(", line 42, in greet").unwrap();
        assert_eq!(num, 42);
    }

    #[test]
    fn parse_line_suffix_no_context() {
        let (num, _, _) = parse_line_suffix(", line 7").unwrap();
        assert_eq!(num, 7);
    }

    #[test]
    fn parse_line_suffix_missing_returns_none() {
        assert!(parse_line_suffix(", no line here").is_none());
    }

    // ── apply_line_map ───────────────────────────────────────────────────────

    #[test]
    fn apply_line_map_identity_unchanged() {
        let map = SourceMap {
            source: "m.ty".into(),
            strategy: LineStrategy::Identity,
            lines: vec![],
        };
        let result = apply_line_map(&map, ", line 10, in f");
        assert_eq!(result, ", line 10, in f");
    }

    #[test]
    fn apply_line_map_table_rewrites_line() {
        // Python line 3 → ty line 2 (sugar expanded: two py lines from one ty line).
        let map = SourceMap {
            source: "m.ty".into(),
            strategy: LineStrategy::Table,
            lines: vec![1, 1, 2, 2, 3], // 0-indexed: py_line 1→ty 1, 2→ty 1, 3→ty 2, ...
        };
        let result = apply_line_map(&map, ", line 3, in f");
        assert_eq!(result, ", line 2, in f");
    }

    #[test]
    fn apply_line_map_table_out_of_range_falls_back_to_identity() {
        let map = SourceMap {
            source: "m.ty".into(),
            strategy: LineStrategy::Table,
            lines: vec![1, 2], // only 2 entries
        };
        // Python line 10 is beyond the table; keep 10.
        let result = apply_line_map(&map, ", line 10, in f");
        assert_eq!(result, ", line 10, in f");
    }

    // ── rewrite_traceback ────────────────────────────────────────────────────

    #[test]
    fn rewrite_traceback_leaves_non_frame_lines_alone() {
        let text = "Traceback (most recent call last):\nSomeError: oops\n";
        let result = rewrite_traceback(text, None);
        assert_eq!(result, text);
    }

    #[test]
    fn rewrite_traceback_leaves_frame_without_map_alone() {
        // No .py.map next to this imaginary file → pass through unchanged.
        let text = "  File \"/no/such/file.py\", line 1, in <module>\n";
        let result = rewrite_traceback(text, None);
        assert_eq!(result, text);
    }

    #[test]
    fn rewrite_traceback_rewrites_frame_with_adjacent_map() {
        // Write a real .py.map into a temp dir and point the .py path there.
        let dir = tempfile::tempdir().unwrap();
        let py_path = dir.path().join("main.py");
        let map_path = dir.path().join("main.py.map");
        std::fs::write(&py_path, "").unwrap();
        std::fs::write(
            &map_path,
            r#"{"version":1,"source":"main.ty","line_strategy":"identity"}"#,
        )
        .unwrap();

        let text = format!("  File \"{}\", line 5, in greet\n", py_path.display());
        let result = rewrite_traceback(&text, None);

        assert!(
            result.contains("main.ty"),
            "expected Typhon source in output; got: {result}"
        );
        assert!(
            result.contains("line 5"),
            "line number should be preserved; got: {result}"
        );
        assert!(
            !result.contains("main.py\""),
            "original .py path should be gone; got: {result}"
        );
    }

    #[test]
    fn rewrite_traceback_table_strategy_remaps_line() {
        let dir = tempfile::tempdir().unwrap();
        let py_path = dir.path().join("app.py");
        let map_path = dir.path().join("app.py.map");
        std::fs::write(&py_path, "").unwrap();
        // Python line 3 → Typhon line 2 (sugar expansion produced an extra line).
        std::fs::write(
            &map_path,
            r#"{"version":2,"source":"app.ty","line_strategy":"table","lines":[1,1,2,2,3]}"#,
        )
        .unwrap();

        let text = format!("  File \"{}\", line 3, in parse\n", py_path.display());
        let result = rewrite_traceback(&text, None);
        assert!(
            result.contains("line 2"),
            "line 3 in Python should map to line 2 in Typhon; got: {result}"
        );
    }

    #[test]
    fn rewrite_traceback_shows_the_ty_source_row() {
        // Rewriting the frame header but leaving CPython's row under it
        // showed emitted Python — including generated names like
        // `__typhon_qi_0__` — under a `.ty` path and line, so opening that
        // line showed something else entirely. The row is substituted, and
        // the column anchors, which no longer line up, are dropped.
        let dir = tempfile::tempdir().unwrap();
        let py = dir.path().join("main.py");
        let map = dir.path().join("main.py.map");
        let ty = dir.path().join("main.ty");
        std::fs::write(&py, "").unwrap();
        std::fs::write(
            &ty,
            "def go() -> int:\n    let v: int = parse(b)?\n    return v\n",
        )
        .unwrap();
        std::fs::write(
            &map,
            r#"{"version":2,"source":"main.ty","line_strategy":"table","lines":[1,2,2,3]}"#,
        )
        .unwrap();

        let text = format!(
            "Traceback (most recent call last):\n  File \"{py}\", line 3, in go\n    __typhon_qi_0__ = parse(b)\n    ~~~~~~~^^^^\nValueError: bad\n",
            py = py.display()
        );
        let result = rewrite_traceback(&text, None);

        assert!(result.contains("main.ty"), "path rewritten:\n{result}");
        assert!(result.contains("line 2"), "line remapped:\n{result}");
        assert!(
            result.contains("let v: int = parse(b)?"),
            "the .ty row should be shown:\n{result}"
        );
        assert!(
            !result.contains("__typhon_qi_0__"),
            "the generated name must not survive:\n{result}"
        );
        assert!(
            !result.contains("~~~~~~~^^^^"),
            "anchors into the old row must be dropped:\n{result}"
        );
        assert!(
            result.contains("ValueError: bad"),
            "message kept:\n{result}"
        );
    }

    #[test]
    fn rewrite_traceback_full_example() {
        let dir = tempfile::tempdir().unwrap();
        let py = dir.path().join("main.py");
        let map = dir.path().join("main.py.map");
        std::fs::write(&py, "").unwrap();
        std::fs::write(
            &map,
            r#"{"version":1,"source":"main.ty","line_strategy":"identity"}"#,
        )
        .unwrap();

        let text = format!(
            "Traceback (most recent call last):\n  File \"{py}\", line 10, in main\n    greet()\nTypeError: bad arg\n",
            py = py.display()
        );
        let result = rewrite_traceback(&text, None);

        assert!(result.starts_with("Traceback"), "header preserved");
        assert!(result.contains("main.ty"), "path rewritten");
        assert!(result.contains("line 10"), "line preserved");
        assert!(result.contains("greet()"), "code line preserved");
        assert!(result.contains("TypeError"), "exception preserved");
    }
}
