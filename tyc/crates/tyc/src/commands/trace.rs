//! `tyc trace` — map a Python traceback back to Typhon source.
//!
//! Reads a Python traceback (either from a path argument or stdin) and
//! rewrites every `File "…/build/foo.py", line N` reference to point at the
//! original `.ty` source.  The mapping is recorded in a sibling `.py.map`
//! file emitted by `tyc build`.
//!
//! ## v1 map format
//!
//! Each `<name>.py.map` is a single-line JSON object:
//!
//! ```text
//! {"version":1,"source":"src/foo.ty","line_strategy":"identity"}
//! ```
//!
//! Only the `source` field is consumed by `tyc trace` today; line numbers
//! are forwarded verbatim because most Typhon → Python transformations
//! preserve line counts (val/var stripping, `?` sugar, comptime, lazy val).
//! Constructs that emit multiple lines (`with`-chains, `gather:`, `?`
//! propagation) may report a line offset by a small amount; full mapping
//! support is tracked as a follow-up.

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

    /// Directory containing `.py.map` source-map files.  When omitted, the
    /// map for `foo.py` is looked up next to it.
    #[arg(long, value_name = "DIR")]
    pub map_dir: Option<PathBuf>,
}

pub fn run(args: TraceArgs) -> Result<()> {
    let input = match &args.traceback {
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

    let rewritten = rewrite_traceback(&input, args.map_dir.as_deref());
    print!("{}", rewritten);
    Ok(())
}

/// Rewrite every `File "…/foo.py", line N` reference in `traceback` so that
/// it points at the original `.ty` source.  Lines that are not traceback
/// entries pass through unchanged.
pub fn rewrite_traceback(traceback: &str, map_dir: Option<&Path>) -> String {
    let mut out = String::with_capacity(traceback.len());
    for line in traceback.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if let Some(rewritten) = rewrite_traceback_line(trimmed, map_dir) {
            out.push_str(&rewritten);
            // Preserve the original terminator.
            let tail = &line[trimmed.len()..];
            out.push_str(tail);
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Try to rewrite a single `File "…/foo.py", line N` line.  Returns
/// `Some(replacement)` when the line matches the traceback shape and the
/// corresponding `.py.map` was located; otherwise `None`.
fn rewrite_traceback_line(line: &str, map_dir: Option<&Path>) -> Option<String> {
    // Locate `File "` (allow surrounding indentation produced by CPython).
    let needle = "File \"";
    let prefix_end = line.find(needle)?;
    let after_quote = prefix_end + needle.len();
    let close_quote = line[after_quote..].find('"')?;
    let path_str = &line[after_quote..after_quote + close_quote];

    // The traceback path must end in `.py` for us to attempt a rewrite.
    if !path_str.ends_with(".py") {
        return None;
    }

    let py_path = PathBuf::from(path_str);

    // Locate the `.py.map`: explicit --map-dir wins, else look next to the file.
    let map_path = match map_dir {
        Some(dir) => {
            let base = py_path.file_name().map(PathBuf::from)?;
            dir.join(base).with_extension("py.map")
        }
        None => py_path.with_extension("py.map"),
    };

    let map_text = std::fs::read_to_string(&map_path).ok()?;
    let source = parse_source_field(&map_text)?;

    // Rebuild the line with the rewritten path; preserve the rest verbatim.
    let mut out = String::with_capacity(line.len() + source.len());
    out.push_str(&line[..after_quote]);
    out.push_str(&source);
    out.push_str(&line[after_quote + close_quote..]);
    Some(out)
}

/// Extract the value of the `"source"` key from a minimal one-line JSON
/// map.  Avoids pulling in a full JSON dependency for what is intentionally
/// a fixed-shape file.
fn parse_source_field(text: &str) -> Option<String> {
    let key = "\"source\"";
    let key_pos = text.find(key)?;
    let after = &text[key_pos + key.len()..];
    let colon = after.find(':')?;
    let after_colon = &after[colon + 1..];
    let open = after_colon.find('"')?;
    let rest = &after_colon[open + 1..];
    // Find the closing quote, skipping `\"` and `\\` escapes.
    let mut value = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(value),
            '\\' => {
                if let Some(esc) = chars.next() {
                    value.push(esc);
                }
            }
            _ => value.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_source_field_extracts_value() {
        let text = "{\"version\":1,\"source\":\"src/foo.ty\",\"line_strategy\":\"identity\"}";
        assert_eq!(parse_source_field(text).as_deref(), Some("src/foo.ty"));
    }

    #[test]
    fn parse_source_field_handles_escaped_quote() {
        let text = "{\"source\":\"src/with\\\"quote.ty\"}";
        assert_eq!(
            parse_source_field(text).as_deref(),
            Some("src/with\"quote.ty")
        );
    }

    #[test]
    fn parse_source_field_returns_none_when_missing() {
        let text = "{\"version\":1}";
        assert!(parse_source_field(text).is_none());
    }

    #[test]
    fn rewrite_traceback_line_replaces_py_with_ty() {
        let tmp = tempfile::tempdir().unwrap();
        let py = tmp.path().join("foo.py");
        std::fs::write(&py, "x = 1\n").unwrap();
        std::fs::write(
            tmp.path().join("foo.py.map"),
            "{\"version\":1,\"source\":\"src/foo.ty\",\"line_strategy\":\"identity\"}\n",
        )
        .unwrap();
        let line = format!("  File \"{}\", line 7, in main", py.display());
        let result = rewrite_traceback_line(&line, None).unwrap();
        assert!(
            result.contains("src/foo.ty"),
            "rewritten line should contain .ty path; got: {result}"
        );
        assert!(
            !result.contains("foo.py\""),
            "rewritten line should not contain the .py path; got: {result}"
        );
    }

    #[test]
    fn rewrite_traceback_line_passes_through_when_map_absent() {
        let line = "  File \"/no/such/path.py\", line 1, in main";
        assert!(
            rewrite_traceback_line(line, None).is_none(),
            "missing map should return None so the original line is preserved"
        );
    }

    #[test]
    fn rewrite_traceback_full_input_preserves_non_file_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let py = tmp.path().join("foo.py");
        std::fs::write(&py, "x = 1\n").unwrap();
        std::fs::write(
            tmp.path().join("foo.py.map"),
            "{\"version\":1,\"source\":\"src/foo.ty\",\"line_strategy\":\"identity\"}\n",
        )
        .unwrap();
        let input = format!(
            "Traceback (most recent call last):\n  File \"{}\", line 7, in main\n    raise ValueError(\"boom\")\nValueError: boom\n",
            py.display()
        );
        let out = rewrite_traceback(&input, None);
        assert!(
            out.contains("Traceback (most recent call last):"),
            "header line must be preserved"
        );
        assert!(
            out.contains("src/foo.ty"),
            ".ty path must appear in rewritten output; got:\n{out}"
        );
        assert!(
            out.contains("ValueError: boom"),
            "exception message must be preserved"
        );
    }

    #[test]
    fn rewrite_traceback_uses_map_dir_when_provided() {
        let tmp = tempfile::tempdir().unwrap();
        let map_dir = tmp.path().join("maps");
        std::fs::create_dir_all(&map_dir).unwrap();
        std::fs::write(
            map_dir.join("foo.py.map"),
            "{\"version\":1,\"source\":\"other/foo.ty\"}\n",
        )
        .unwrap();
        let line = "  File \"/build/foo.py\", line 3, in main";
        let result = rewrite_traceback_line(line, Some(&map_dir)).unwrap();
        assert!(result.contains("other/foo.ty"), "got: {result}");
    }
}
