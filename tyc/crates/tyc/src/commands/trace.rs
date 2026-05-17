//! `tyc trace` — map a Python traceback back to Typhon source.
//!
//! Reads a Python traceback (from a file or stdin) and rewrites every
//! `  File "path/to/foo.py", line N, in func` frame that has an adjacent
//! `.py.map` sidecar. The sidecar encodes the corresponding `.ty` source
//! path and, in v2 maps, a per-line mapping table for sugar-expanded
//! constructs (`?`, `gather:`, `with`-chains).

use std::io::Read;
use std::path::{Path, PathBuf};

use clap::Args;
use miette::{miette, Result};

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
pub fn rewrite_traceback(text: &str, map_dir: Option<&Path>) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        out.push_str(&try_rewrite_frame(line, map_dir));
        out.push('\n');
    }
    // Preserve trailing-newline parity with the original.
    if !text.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    out
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

// ── source map parsing ────────────────────────────────────────────────────────

#[derive(Debug)]
struct SourceMap {
    source: String,
    strategy: LineStrategy,
    /// v2 line table: `lines[py_line_0indexed]` = ty_line (1-indexed).
    lines: Vec<u32>,
}

#[derive(Debug, PartialEq)]
enum LineStrategy {
    Identity,
    Table,
}

/// Parse a `.py.map` JSON blob.
///
/// V1 format: `{"version":1,"source":"rel/path.ty","line_strategy":"identity"}`
/// V2 format: adds `,"lines":[1,1,2,3,3,...]`
fn parse_map(body: &str) -> Option<SourceMap> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let obj = v.as_object()?;
    let source = obj.get("source")?.as_str()?.to_owned();
    let strategy_str = obj
        .get("line_strategy")
        .and_then(|v| v.as_str())
        .unwrap_or("identity");
    let strategy = if strategy_str == "table" {
        LineStrategy::Table
    } else {
        LineStrategy::Identity
    };
    let lines = obj
        .get("lines")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_u64().map(|n| n as u32))
                .collect()
        })
        .unwrap_or_default();
    Some(SourceMap {
        source,
        strategy,
        lines,
    })
}

// ── file lookup ───────────────────────────────────────────────────────────────

/// Find and read the `.py.map` sidecar for `py_path`.
///
/// Search order:
///   1. `<py_path>.map` adjacent to the `.py` file.
///   2. `<map_dir>/<filename>.map` when `--map-dir` was given.
fn load_map_for(py_path: &str, map_dir: Option<&Path>) -> Option<String> {
    // Adjacent sidecar: foo.py → foo.py.map.
    let adjacent = PathBuf::from(format!("{py_path}.map"));
    if adjacent.exists() {
        return std::fs::read_to_string(&adjacent).ok();
    }

    if let Some(dir) = map_dir {
        if let Some(base) = Path::new(py_path).file_name() {
            let candidate = dir.join(format!("{}.map", base.to_string_lossy()));
            if candidate.exists() {
                return std::fs::read_to_string(&candidate).ok();
            }
        }
    }

    None
}

// ── path resolution ───────────────────────────────────────────────────────────

/// Resolve the Typhon source path for a given `.py` path and map `source`.
///
/// Walks up from the `.py` file's directory to find `typhon.toml` using the
/// existing [`crate::config::TyphonConfig::load`] loader, then constructs
/// `<project_root>/<src_dir>/<source>`.  Falls back to returning `source`
/// as-is when the project root cannot be determined.
fn resolve_ty_path(py_path: &str, source: &str) -> String {
    let py = Path::new(py_path);
    let start_dir = if py.is_absolute() {
        py.parent().map(|p| p.to_path_buf())
    } else {
        py.canonicalize()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    };

    if let Some(dir) = start_dir {
        if let Ok(Some((toml_path, config))) = crate::config::TyphonConfig::load(&dir) {
            if let Some(root) = toml_path.parent() {
                let ty = root.join(&config.project.src).join(source);
                return ty.to_string_lossy().into_owned();
            }
        }
    }

    // Fallback: show the relative source path from the map.
    source.to_owned()
}

// ── line mapping ──────────────────────────────────────────────────────────────

/// Rewrite the `", line N, in func"` suffix using the map's line strategy.
fn apply_line_map(map: &SourceMap, after_path: &str) -> String {
    let Some((py_line, num_offset, digit_len)) = parse_line_suffix(after_path) else {
        return after_path.to_owned();
    };

    let ty_line = match map.strategy {
        LineStrategy::Identity => py_line,
        LineStrategy::Table => {
            let idx = py_line.saturating_sub(1) as usize;
            map.lines.get(idx).copied().unwrap_or(py_line)
        }
    };

    if ty_line == py_line {
        return after_path.to_owned();
    }

    // Splice the new line number into the original suffix string using the
    // exact span from parsing so the replacement is consistent with how the
    // number was read (avoids any leading-zero / formatting divergence).
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
