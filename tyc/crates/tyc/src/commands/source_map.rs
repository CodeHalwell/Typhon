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
///   1. `<out>/.sourcemaps/<rel>.py.map` — the layout `tyc build` emits.
///      `<out>` is discovered by walking up from `py_path` to the
///      directory that contains a `.sourcemaps/` sibling.
///   2. `<py_path>.map` adjacent to the `.py` file — legacy layout for
///      builds emitted by older `tyc` versions (pre-`.sourcemaps/`).
///   3. `<map_dir>/<filename>.map` when `--map-dir` was given.
pub fn load_map_for(py_path: &str, map_dir: Option<&Path>) -> Option<String> {
    let py = Path::new(py_path);
    // 1. Walk up from the `.py` looking for a directory that contains a
    //    `.sourcemaps/` sibling. The path inside `.sourcemaps/` mirrors
    //    the relative path from that directory to the `.py`, with `.map`
    //    appended — so `build/foo/bar.py` → `build/.sourcemaps/foo/bar.py.map`.
    let mut anchor = py.parent();
    let mut rel_segments: Vec<&std::ffi::OsStr> =
        py.file_name().map(|n| vec![n]).unwrap_or_default();
    while let Some(dir) = anchor {
        let candidate_dir = dir.join(".sourcemaps");
        if candidate_dir.is_dir() {
            let mut candidate = candidate_dir;
            for seg in rel_segments.iter().rev() {
                candidate.push(seg);
            }
            candidate.set_extension("py.map");
            if candidate.exists() {
                return std::fs::read_to_string(&candidate).ok();
            }
        }
        if let Some(name) = dir.file_name() {
            rel_segments.push(name);
        }
        anchor = dir.parent();
    }
    // 2. Legacy adjacent layout.
    let adjacent = PathBuf::from(format!("{py_path}.map"));
    if adjacent.exists() {
        return std::fs::read_to_string(&adjacent).ok();
    }
    // 3. Explicit map-dir override. Covers `.py` references that are
    //    *relative* to the build dir — the form the embedded `ty` renderer
    //    emits (`main.py`, `pkg/mod.py`). Their `parent()` can't anchor the
    //    `.sourcemaps/` walk in step 1 (the subprocess path avoids this by
    //    emitting absolute paths), so resolve them against `map_dir`
    //    directly: first the `.sourcemaps/<rel>.py.map` layout, then the
    //    legacy adjacent `<base>.py.map`.
    if let Some(dir) = map_dir {
        if Path::new(py_path).is_relative() {
            let in_sourcemaps = dir.join(".sourcemaps").join(format!("{py_path}.map"));
            if in_sourcemaps.exists() {
                return std::fs::read_to_string(&in_sourcemaps).ok();
            }
        }
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

    #[test]
    fn load_map_for_finds_sourcemaps_subdir() {
        // The layout `tyc build` writes today: maps live under a
        // dedicated `.sourcemaps/` subtree mirroring the `.py` files
        // so the build output dir stays uncluttered.
        let tmp = tempfile::tempdir().unwrap();
        let build = tmp.path().join("build");
        let sourcemaps = build.join(".sourcemaps");
        std::fs::create_dir_all(&sourcemaps).unwrap();
        let py = build.join("foo.py");
        std::fs::write(&py, "").unwrap();
        let body = r#"{"version":1,"source":"foo.ty","line_strategy":"identity"}"#;
        std::fs::write(sourcemaps.join("foo.py.map"), body).unwrap();
        let loaded = load_map_for(py.to_str().unwrap(), None).expect("map should be found");
        assert!(loaded.contains("foo.ty"));
    }

    #[test]
    fn load_map_for_handles_nested_sourcemaps() {
        // Nested module: `build/sub/foo.py` → `build/.sourcemaps/sub/foo.py.map`.
        let tmp = tempfile::tempdir().unwrap();
        let build = tmp.path().join("build");
        let nested_py_dir = build.join("sub");
        let nested_map_dir = build.join(".sourcemaps").join("sub");
        std::fs::create_dir_all(&nested_py_dir).unwrap();
        std::fs::create_dir_all(&nested_map_dir).unwrap();
        let py = nested_py_dir.join("foo.py");
        std::fs::write(&py, "").unwrap();
        let body = r#"{"version":2,"source":"sub/foo.ty","line_strategy":"table","lines":[1,2]}"#;
        std::fs::write(nested_map_dir.join("foo.py.map"), body).unwrap();
        let loaded = load_map_for(py.to_str().unwrap(), None).expect("nested map should be found");
        assert!(loaded.contains("sub/foo.ty"));
    }

    #[test]
    fn load_map_for_resolves_build_relative_py_via_map_dir() {
        // The embedded `ty` renderer emits build-*relative* refs (`main.py`,
        // `pkg/mod.py`) whose `parent()` can't anchor the `.sourcemaps/`
        // walk. They must resolve against `map_dir/.sourcemaps/<rel>.py.map`.
        let tmp = tempfile::tempdir().unwrap();
        let build = tmp.path().join("build");
        std::fs::create_dir_all(build.join(".sourcemaps").join("pkg")).unwrap();
        let flat = r#"{"version":2,"source":"main.ty","line_strategy":"identity","lines":[]}"#;
        std::fs::write(build.join(".sourcemaps").join("main.py.map"), flat).unwrap();
        let nested = r#"{"version":2,"source":"pkg/mod.ty","line_strategy":"identity","lines":[]}"#;
        std::fs::write(
            build.join(".sourcemaps").join("pkg").join("mod.py.map"),
            nested,
        )
        .unwrap();

        let got = load_map_for("main.py", Some(&build)).expect("relative flat map");
        assert!(got.contains("main.ty"));
        let got_nested = load_map_for("pkg/mod.py", Some(&build)).expect("relative nested map");
        assert!(got_nested.contains("pkg/mod.ty"));
    }

    #[test]
    fn load_map_for_falls_back_to_adjacent_legacy_layout() {
        // Builds emitted by older `tyc` versions wrote the map next to
        // the `.py`. The resolver must still pick those up so existing
        // build dirs work across upgrades.
        let tmp = tempfile::tempdir().unwrap();
        let py = tmp.path().join("foo.py");
        std::fs::write(&py, "").unwrap();
        let body = r#"{"version":1,"source":"foo.ty","line_strategy":"identity"}"#;
        std::fs::write(tmp.path().join("foo.py.map"), body).unwrap();
        let loaded = load_map_for(py.to_str().unwrap(), None).expect("legacy map should be found");
        assert!(loaded.contains("foo.ty"));
    }

    #[test]
    fn load_map_for_prefers_sourcemaps_over_legacy_adjacent() {
        // After upgrading from a pre-`.sourcemaps/` `tyc`, a stale
        // adjacent `<rel>.py.map` will sit alongside the freshly
        // written `.sourcemaps/<rel>.py.map`. The resolver must
        // pick the new one so debugger / trace output reflects the
        // current build, not the stale sidecar.
        let tmp = tempfile::tempdir().unwrap();
        let build = tmp.path().join("build");
        let sourcemaps = build.join(".sourcemaps");
        std::fs::create_dir_all(&sourcemaps).unwrap();
        let py = build.join("foo.py");
        std::fs::write(&py, "").unwrap();
        let fresh = r#"{"version":2,"source":"fresh.ty","line_strategy":"table","lines":[1]}"#;
        let stale = r#"{"version":1,"source":"stale.ty","line_strategy":"identity"}"#;
        std::fs::write(sourcemaps.join("foo.py.map"), fresh).unwrap();
        std::fs::write(build.join("foo.py.map"), stale).unwrap();
        let loaded = load_map_for(py.to_str().unwrap(), None).expect("map should be found");
        assert!(
            loaded.contains("fresh.ty"),
            ".sourcemaps/ must win over legacy; got: {loaded}"
        );
    }
}
