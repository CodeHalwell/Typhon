//! `.py.map` v2 provenance tests.
//!
//! The v2 sidecar's `lines` table claims to map each emitted Python line back
//! to a line of the `.ty` file it was compiled from. Until the sugar-expansion
//! chain grew line maps it recorded lines of the *preprocessed buffer*
//! instead — numbers that routinely ran past the end of the `.ty` file and
//! that spread a single source statement's expansion across several
//! consecutive fake lines. These tests pin the real invariant.
//!
//! Each test drives the real `tyc` binary through `CARGO_BIN_EXE_tyc`, the
//! same way `tests/pipeline.rs` does.

use std::path::Path;
use std::process::Command;

// ── helpers ───────────────────────────────────────────────────────────────────

fn tyc() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tyc"))
}

/// Write a minimal `typhon.toml` + `src/main.ty` under `dir`.
///
/// `format = false` keeps `ruff format` out of the picture: the emitter's
/// `(out_line -> source offset)` table is recorded before formatting, so a
/// reflow would shift the emitted lines out from under the map for reasons
/// that have nothing to do with what these tests measure.
fn scaffold(dir: &Path, src_content: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("typhon.toml"),
        "[project]\nname = \"test\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
         [python]\ntarget = \"3.13\"\n[emit]\nformat = false\n[strictness]\n[env]\n",
    )
    .unwrap();
    std::fs::write(dir.join("src").join("main.ty"), src_content).unwrap();
}

/// Build `dir` and return `(emitted Python lines, map `lines` table)`.
fn build_and_load(dir: &Path) -> (Vec<String>, Vec<usize>) {
    let status = tyc()
        .arg("build")
        .arg("--no-sync")
        .arg(dir)
        .status()
        .unwrap();
    assert!(status.success(), "build should succeed");

    let map_body =
        std::fs::read_to_string(dir.join("build").join(".sourcemaps").join("main.py.map")).unwrap();
    assert!(
        map_body.contains("\"version\":2") && map_body.contains("\"line_strategy\":\"table\""),
        "sidecar must stay v2 / table strategy; got: {map_body}"
    );
    let json: serde_json::Value = serde_json::from_str(&map_body).expect("valid JSON in .py.map");
    let lines: Vec<usize> = json["lines"]
        .as_array()
        .expect("lines array in .py.map")
        .iter()
        .map(|x| x.as_u64().expect("numeric line entry") as usize)
        .collect();

    let py = std::fs::read_to_string(dir.join("build").join("main.py")).unwrap();
    let py_lines: Vec<String> = py.lines().map(|l| l.to_owned()).collect();
    (py_lines, lines)
}

/// 1-based index of the single `.ty` line containing `needle`.
fn ty_line_of(src: &str, needle: &str) -> usize {
    let hits: Vec<usize> = src
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains(needle))
        .map(|(i, _)| i + 1)
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "`{needle}` must occur on exactly one .ty line; found {hits:?}"
    );
    hits[0]
}

/// The map values recorded for every emitted Python line containing `needle`.
fn mapped_lines_for(py: &[String], map: &[usize], needle: &str) -> Vec<usize> {
    py.iter()
        .enumerate()
        .filter(|(_, l)| l.contains(needle))
        .map(|(i, _)| {
            *map.get(i)
                .unwrap_or_else(|| panic!("map has no entry for emitted line {}", i + 1))
        })
        .collect()
}

/// Every entry must name a line that actually exists in the `.ty` file.
fn assert_in_range(map: &[usize], src: &str) {
    let ty_lines = src.lines().count();
    for (i, &v) in map.iter().enumerate() {
        assert!(
            v >= 1 && v <= ty_lines,
            "map entry for emitted line {} is {v}, outside the .ty file's 1..={ty_lines}",
            i + 1
        );
    }
}

// ── `?` propagation ───────────────────────────────────────────────────────────

#[test]
fn question_op_expansion_all_maps_to_its_single_ty_line() {
    // 1: def parse(s: str) -> Result[int, str]:
    // 2:     let n = int(s)?
    // 3:     return Ok(n)
    //
    // Line 2 lowers to four Python lines (temp assign, isinstance guard,
    // early return, unwrap). All four must name line 2 — not four consecutive
    // lines of the preprocessed buffer, and never a line past the end of a
    // three-line file.
    const SRC: &str = "\
def parse(s: str) -> Result[int, str]:
    let n = int(s)?
    return Ok(n)
";
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), SRC);
    let (py, map) = build_and_load(tmp.path());

    assert_in_range(&map, SRC);

    // Every line of the `?` expansion shares one source line.
    let q_lines = mapped_lines_for(&py, &map, "__typhon_q_");
    assert!(
        q_lines.len() >= 4,
        "the `?` lowering should emit at least four lines; got {q_lines:?}"
    );
    let expected = ty_line_of(SRC, "let n = int(s)?");
    assert!(
        q_lines.iter().all(|&v| v == expected),
        "every `?`-expansion line must map to .ty line {expected}; got {q_lines:?}"
    );

    // The statements either side keep their own lines.
    assert_eq!(
        mapped_lines_for(&py, &map, "def parse(s: str)"),
        vec![ty_line_of(SRC, "def parse(s: str)")],
        "the function header must map to its own line"
    );
    assert_eq!(
        mapped_lines_for(&py, &map, "return Ok(n)"),
        vec![ty_line_of(SRC, "return Ok(n)")],
        "the trailing return must map to its own line"
    );
}

// ── `gather:` and `with`-chains ───────────────────────────────────────────────

#[test]
fn gather_and_with_chain_map_back_to_their_own_ty_lines() {
    // Both of these consume a block of source and emit a differently-shaped
    // one, so both used to smear the map across preprocessed-buffer lines.
    const SRC: &str = "\
import asyncio

def half(n: int) -> Result[int, str]:
    if n % 2 != 0:
        return Err(\"odd\")
    return Ok(n // 2)

def combine(a: int, b: int) -> Result[int, str]:
    with x = half(a)?,
         y = half(b)?:
        let total: int = x + y
        return Ok(total)
    else err:
        print(err)
        return Err(err)

@gatherable
async def fetch_a() -> int:
    await asyncio.sleep(0)
    return 1

@gatherable
async def fetch_b() -> int:
    await asyncio.sleep(0)
    return 2

async def load() -> int:
    gather:
        a = fetch_a()
        b = fetch_b()
    return a + b
";
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), SRC);
    let (py, map) = build_and_load(tmp.path());

    assert_in_range(&map, SRC);

    // `with`-chain: each binding's unwrap ladder names the binding's own line.
    let first = ty_line_of(SRC, "with x = half(a)?,");
    let second = ty_line_of(SRC, "y = half(b)?:");
    let ladder0 = mapped_lines_for(&py, &map, "__typhon_with_0__");
    let ladder1 = mapped_lines_for(&py, &map, "__typhon_with_1__");
    assert!(
        !ladder0.is_empty() && ladder0.iter().all(|&v| v == first),
        "the first `with` binding's lowering must map to .ty line {first}; got {ladder0:?}"
    );
    assert!(
        !ladder1.is_empty() && ladder1.iter().all(|&v| v == second),
        "the second `with` binding's lowering must map to .ty line {second}; got {ladder1:?}"
    );

    // The `else err:` body is copied into every binding's guard; each copy
    // must point back at the one line the user wrote it on.
    let else_print = ty_line_of(SRC, "print(err)");
    let printed = mapped_lines_for(&py, &map, "print(__typhon_with_err_");
    assert_eq!(
        printed.len(),
        2,
        "the else-body print should be copied once per binding; got {printed:?}"
    );
    assert!(
        printed.iter().all(|&v| v == else_print),
        "each else-body copy must map to .ty line {else_print}; got {printed:?}"
    );

    // The success body keeps its own lines rather than collapsing onto the
    // `with` header.
    assert_eq!(
        mapped_lines_for(&py, &map, "total: int = x + y"),
        vec![ty_line_of(SRC, "let total: int = x + y")],
        "the `with` body must keep its own line"
    );

    // `gather:`: each `create_task` names the binding it came from.
    let a_line = ty_line_of(SRC, "a = fetch_a()");
    let b_line = ty_line_of(SRC, "b = fetch_b()");
    assert_eq!(
        mapped_lines_for(&py, &map, "create_task(fetch_a())"),
        vec![a_line],
        "the first gather binding must map to its own line"
    );
    assert_eq!(
        mapped_lines_for(&py, &map, "create_task(fetch_b())"),
        vec![b_line],
        "the second gather binding must map to its own line"
    );
    assert_eq!(
        mapped_lines_for(&py, &map, "return a + b"),
        vec![ty_line_of(SRC, "return a + b")],
        "the statement after the gather block must keep its own line"
    );
}

// ── the rest of the sugar chain ───────────────────────────────────────────────

#[test]
fn pipes_guards_rescue_and_typed_unpack_stay_in_range() {
    // A smoke test over the remaining line-count-changing passes: multi-line
    // `guard`, `|>` pipes, typed tuple unpack, `as!`, and both `rescue` forms.
    // Each injects or removes lines, so each is a chance for the table to
    // point outside the file.
    const SRC: &str = "\
def double(n: int) -> int:
    return n * 2

def describe(raw: str?) -> Result[str, str]:
    guard value = raw else:
        return Err(\"missing\")
    let n: int = int(value) rescue e: f\"bad: {e}\"
    let scaled: int = n |> double() |> double()
    let (label: str, width: int) = (\"n\", scaled)
    return Ok(f\"{label}={width}\")

def load(payload: dict[str, str]) -> Result[int, str]:
    rescue e: f\"boom: {e}\":
        let raw: str = payload[\"n\"] as! str
        return Ok(int(raw))
";
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), SRC);
    let (py, map) = build_and_load(tmp.path());

    assert_in_range(&map, SRC);
    assert_eq!(
        map.len(),
        py.len(),
        "the table must carry one entry per emitted Python line"
    );

    // Spot-check two statements that survive lowering verbatim.
    assert_eq!(
        mapped_lines_for(&py, &map, "return n * 2"),
        vec![ty_line_of(SRC, "return n * 2")],
        "an untouched statement must map to its own line"
    );
    assert_eq!(
        mapped_lines_for(&py, &map, "double(double(n))"),
        vec![ty_line_of(SRC, "|> double() |> double()")],
        "a lowered pipe chain must map to the line it was written on"
    );
}
