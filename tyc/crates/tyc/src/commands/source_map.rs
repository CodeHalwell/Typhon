//! Shared `.py.map` source-map loader and line-mapping helpers.
//!
//! Used by `tyc trace` (Python tracebacks → Typhon source) and
//! `tyc ty` (`ty` diagnostics → Typhon source). The map format is
//! emitted by `tyc build` alongside every `.py` artefact — see
//! `commands/build.rs` for the producer.

use std::path::{Path, PathBuf};

/// Parsed `.py.map` JSON.
#[derive(Debug)]
pub struct SourceMap {
    /// Relative path to the `.ty` source recorded in the map.
    pub source: String,
    pub strategy: LineStrategy,
    /// v2 line table: `lines[py_line_0indexed]` = ty_line (1-indexed).
    pub lines: Vec<u32>,
}

#[derive(Debug, PartialEq)]
pub enum LineStrategy {
    Identity,
    Table,
}

/// Parse a `.py.map` JSON blob.
///
/// V1 format: `{"version":1,"source":"rel/path.ty","line_strategy":"identity"}`
/// V2 format: adds `,"lines":[1,1,2,3,3,...]`
pub fn parse_map(body: &str) -> Option<SourceMap> {
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

/// Find and read the `.py.map` sidecar for `py_path`.
///
/// Search order:
///   1. `<py_path>.map` adjacent to the `.py` file.
///   2. `<map_dir>/<filename>.map` when `--map-dir` was given.
pub fn load_map_for(py_path: &str, map_dir: Option<&Path>) -> Option<String> {
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

/// Translate a Python line (1-indexed) into a Typhon line using the map.
pub fn map_py_line(map: &SourceMap, py_line: u32) -> u32 {
    match map.strategy {
        LineStrategy::Identity => py_line,
        LineStrategy::Table => {
            let idx = py_line.saturating_sub(1) as usize;
            map.lines.get(idx).copied().unwrap_or(py_line)
        }
    }
}

/// Resolve the Typhon source path for a given `.py` path and map `source`.
///
/// Walks up from the `.py` file's directory to find `typhon.toml`, then
/// constructs `<project_root>/<src_dir>/<source>`. Falls back to returning
/// `source` as-is when the project root cannot be determined.
pub fn resolve_ty_path(py_path: &str, source: &str) -> String {
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

    source.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_v1_map() {
        let body = r#"{"version":1,"source":"main.ty","line_strategy":"identity"}"#;
        let map = parse_map(body).expect("parse");
        assert_eq!(map.source, "main.ty");
        assert_eq!(map.strategy, LineStrategy::Identity);
    }

    #[test]
    fn parse_v2_map() {
        let body = r#"{"version":2,"source":"x.ty","line_strategy":"table","lines":[1,2,3]}"#;
        let map = parse_map(body).expect("parse");
        assert_eq!(map.strategy, LineStrategy::Table);
        assert_eq!(map.lines, vec![1, 2, 3]);
    }

    #[test]
    fn map_py_line_table_strategy() {
        let map = SourceMap {
            source: "x.ty".into(),
            strategy: LineStrategy::Table,
            lines: vec![1, 1, 2, 3, 3],
        };
        assert_eq!(map_py_line(&map, 1), 1);
        assert_eq!(map_py_line(&map, 3), 2);
        assert_eq!(map_py_line(&map, 5), 3);
        // Out of range: identity fallback.
        assert_eq!(map_py_line(&map, 99), 99);
    }

    #[test]
    fn map_py_line_identity_strategy() {
        let map = SourceMap {
            source: "x.ty".into(),
            strategy: LineStrategy::Identity,
            lines: vec![],
        };
        assert_eq!(map_py_line(&map, 42), 42);
    }
}
