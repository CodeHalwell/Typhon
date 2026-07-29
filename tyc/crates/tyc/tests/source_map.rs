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
/// `format = false` isolates the emitter's own `(out_line -> source
/// offset)` table. The formatting stage is covered separately by
/// [`scaffold_formatted`].
fn scaffold(dir: &Path, src_content: &str) {
    scaffold_with_format(dir, src_content, false);
}

/// Like [`scaffold`] but with `[emit] format = true` — the default whenever
/// `ruff` is on `$PATH`, and the configuration the shipped artifacts are
/// built with.
fn scaffold_formatted(dir: &Path, src_content: &str) {
    scaffold_with_format(dir, src_content, true);
}

fn scaffold_with_format(dir: &Path, src_content: &str, format: bool) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let format = if format { "true" } else { "false" };
    std::fs::write(
        dir.join("typhon.toml"),
        format!(
            "[project]\nname = \"test\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
             [python]\ntarget = \"3.13\"\n[emit]\nformat = {format}\n[strictness]\n[env]\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("src").join("main.ty"), src_content).unwrap();
}

/// Whether a `ruff` binary is on `$PATH`.
///
/// The formatting tests below assert facts that hold with or without it,
/// but only `ruff format` actually *reflows* — wraps a long signature,
/// joins a short call — so the assertions that pin the reflow behaviour
/// are gated on its presence rather than silently weakening.
///
/// **`TYC_REQUIRE_RUFF=1` turns the skip into a panic** (the ruff analog of
/// `TYC_REQUIRE_PYTHON`): CI sets it so a runner without ruff fails loudly
/// instead of passing with every reflow/re-key assertion inert — a skip
/// nothing observes is indistinguishable from a pass.
fn ruff_available() -> bool {
    let available = Command::new("ruff")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !available && std::env::var_os("TYC_REQUIRE_RUFF").is_some() {
        panic!(
            "TYC_REQUIRE_RUFF is set but no `ruff` binary was found on PATH. \
             The format-stage source-map assertions only exercise the reflow/re-key \
             logic when ruff actually reflows the output; skipping them would report \
             a pass for a tier of the suite that never ran."
        );
    }
    available
}

/// Locate a Python 3.12+ interpreter, mirroring `tests/build_features.rs`.
/// `TYC_REQUIRE_PYTHON` turns the skip into a panic so CI cannot report a
/// pass for a test tier that never ran.
fn python() -> Option<String> {
    for candidate in ["python3.13", "python3.12", "python3"] {
        let Ok(out) = Command::new(candidate).arg("--version").output() else {
            continue;
        };
        if !out.status.success() {
            continue;
        }
        let v = String::from_utf8_lossy(&out.stdout);
        if let Some(minor) = v
            .trim()
            .strip_prefix("Python 3.")
            .and_then(|rest| rest.split('.').next())
            .and_then(|m| m.parse::<u32>().ok())
        {
            if minor >= 12 {
                return Some(candidate.to_owned());
            }
        }
    }
    if std::env::var_os("TYC_REQUIRE_PYTHON").is_some() {
        panic!(
            "TYC_REQUIRE_PYTHON is set but no Python 3.12+ interpreter was found on PATH. \
             This test executes the emitted Python; skipping it would report a pass for a \
             tier of the suite that never ran."
        );
    }
    None
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

// ── sub-statement granularity ────────────────────────────────────────────────

#[test]
fn match_case_headers_map_to_their_own_ty_line() {
    // The emitter recorded the source offset that was "active" when a line
    // was printed, and only `emit_stmt` ever set one — so a `case` clause
    // header inherited the offset of the *preceding arm's last statement*.
    // `case Square(side):` resolved to the `return` above it.
    const SRC: &str = "\
class Circle frozen:
    radius: float

class Square frozen:
    side: float

type Shape = Circle | Square

def area(s: Shape) -> float:
    match s:
        case Circle(r):
            return 3.14 * r * r
        case Square(side):
            return side * side
";
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), SRC);
    let (py, map) = build_and_load(tmp.path());

    assert_in_range(&map, SRC);
    assert_eq!(
        mapped_lines_for(&py, &map, "case Circle(r):"),
        vec![ty_line_of(SRC, "case Circle(r):")],
        "the first `case` header must name its own line, not `match s:`"
    );
    assert_eq!(
        mapped_lines_for(&py, &map, "case Square(side):"),
        vec![ty_line_of(SRC, "case Square(side):")],
        "the second `case` header must name its own line, not the `return` above it"
    );
    // The arm bodies must not have moved.
    assert_eq!(
        mapped_lines_for(&py, &map, "return 3.14 * r * r"),
        vec![ty_line_of(SRC, "return 3.14 * r * r")]
    );
    assert_eq!(
        mapped_lines_for(&py, &map, "return side * side"),
        vec![ty_line_of(SRC, "return side * side")]
    );
}

#[test]
fn clause_headers_and_decorators_map_to_their_own_ty_line() {
    // Same defect, the rest of the compound-statement surface: `elif` /
    // `else` clause headers, `except` handlers, and each decorator in a
    // stack. Ruff's `StmtFunctionDef.range` starts at the first `@`, so
    // without a per-decorator offset the whole stack *and* the `def` line
    // collapsed onto the first decorator.
    const SRC: &str = "\
import functools

@functools.cache
@functools.wraps(int)
def classify(n: int) -> str:
    if n < 0:
        return \"neg\"
    elif n == 0:
        return \"zero\"
    else:
        return \"pos\"

def guarded(raw: str) -> int:
    try:
        return int(raw)
    except ValueError:
        return -1
    except TypeError:
        return -2
";
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), SRC);
    let (py, map) = build_and_load(tmp.path());

    assert_in_range(&map, SRC);

    for needle in [
        "@functools.cache",
        "@functools.wraps(int)",
        "elif n == 0:",
        "except ValueError:",
        "except TypeError:",
    ] {
        assert_eq!(
            mapped_lines_for(&py, &map, needle),
            vec![ty_line_of(SRC, needle)],
            "`{needle}` must map to the line it was written on"
        );
    }

    // The `def` header follows its decorator stack rather than collapsing
    // onto the first `@`.
    assert_eq!(
        mapped_lines_for(&py, &map, "def classify(n: int)"),
        vec![ty_line_of(SRC, "def classify(n: int)")],
        "the `def` line must map to itself, not to the first decorator"
    );
    // The `else:` of the if-chain names its own line. It shares the token
    // with nothing else in this file, so match on the exact indented form.
    let else_lines: Vec<usize> = py
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trim_end() == "    else:")
        .map(|(i, _)| map[i])
        .collect();
    assert_eq!(
        else_lines,
        vec![ty_line_of(SRC, "    else:")],
        "the `else:` header must name its own line, not the `return` above it"
    );
}

// ── the formatting stage ─────────────────────────────────────────────────────

/// A program whose emitted Python `ruff format` demonstrably reflows: the
/// signature is too long for one line and so is the `total` expression.
/// Everything after the first reflow used to be shifted in the sidecar.
const REFLOW_SRC: &str = "\
def widget(alpha: int, beta: int, gamma: int, delta: int, epsilon: int, zeta: int) -> int:
    return alpha + beta + gamma + delta + epsilon + zeta

def compute() -> int:
    let total: int = widget(1111111, 2222222, 3333333, 4444444, 5555555, 6666666) + widget(7, 8, 9, 10, 11, 12)
    return total

def boom() -> int:
    raise ValueError(\"kaboom\")

def main() -> None:
    print(compute())
    print(boom())

if __name__ == \"__main__\":
    main()
";

#[test]
fn map_is_keyed_to_the_formatted_file_not_the_emitted_one() {
    // With `[emit] format = true` the sidecar was built from the offsets the
    // printer recorded *before* `ruff format` ran. A single wrapped call
    // therefore shifted every later entry relative to the file on disk —
    // and the table came out shorter than the file it described.
    let tmp = tempfile::tempdir().unwrap();
    scaffold_formatted(tmp.path(), REFLOW_SRC);
    let (py, map) = build_and_load(tmp.path());

    assert_eq!(
        map.len(),
        py.len(),
        "the table must carry one entry per line of the file actually written"
    );
    assert_in_range(&map, REFLOW_SRC);

    if ruff_available() {
        assert!(
            py.iter().any(|l| l.trim_end() == "def widget("),
            "this test is only meaningful when ruff reflows the signature; got:\n{}",
            py.join("\n")
        );
    }

    // Statements *after* the reflow are where the drift showed up.
    assert_eq!(
        mapped_lines_for(&py, &map, "raise ValueError(\"kaboom\")"),
        vec![ty_line_of(REFLOW_SRC, "raise ValueError(\"kaboom\")")],
        "a statement after a reflow must still name its own line"
    );
    assert_eq!(
        mapped_lines_for(&py, &map, "print(boom())"),
        vec![ty_line_of(REFLOW_SRC, "print(boom())")],
    );
    assert_eq!(
        mapped_lines_for(&py, &map, "def boom()"),
        vec![ty_line_of(REFLOW_SRC, "def boom()")],
    );

    // Every line ruff wrapped a single statement across must point back at
    // that one statement.
    let wrapped = mapped_lines_for(&py, &map, "7, 8, 9, 10, 11, 12");
    let total_line = ty_line_of(REFLOW_SRC, "let total: int =");
    assert!(
        !wrapped.is_empty() && wrapped.iter().all(|&v| v == total_line),
        "a wrapped continuation must map to .ty line {total_line}; got {wrapped:?}"
    );
}

#[test]
fn tyc_trace_reports_the_raising_ty_line_for_a_formatted_build() {
    // The end-to-end contract. A table that looks right but whose
    // `tyc trace` output is wrong is not a fix, so drive a real CPython
    // traceback through the real `tyc trace`.
    let Some(py_bin) = python() else {
        eprintln!("skipping: no Python 3.12+ interpreter on PATH");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    scaffold_formatted(tmp.path(), REFLOW_SRC);
    let status = tyc()
        .arg("build")
        .arg("--no-sync")
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(status.success(), "build should succeed");

    let run = Command::new(&py_bin)
        .arg(tmp.path().join("build").join("main.py"))
        .output()
        .unwrap();
    assert!(
        !run.status.success(),
        "the program is expected to die with ValueError"
    );
    let traceback = String::from_utf8_lossy(&run.stderr).into_owned();
    let tb_path = tmp.path().join("traceback.txt");
    std::fs::write(&tb_path, &traceback).unwrap();

    let traced = tyc()
        .arg("trace")
        .arg(&tb_path)
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(traced.status.success(), "tyc trace should succeed");
    let remapped = String::from_utf8_lossy(&traced.stdout).into_owned();

    // Every frame must land on the `.ty` line that actually produced it.
    for needle in [
        "raise ValueError(\"kaboom\")",
        "print(boom())",
        "    main()",
    ] {
        let expected = ty_line_of(REFLOW_SRC, needle);
        assert!(
            remapped.contains(&format!("main.ty\", line {expected},")),
            "tyc trace must place a frame at .ty line {expected} (the line holding \
             `{}`); got:\n{remapped}",
            needle.trim()
        );
    }
    assert!(
        !remapped.contains("build/main.py"),
        "every frame should have been remapped to .ty; got:\n{remapped}"
    );
}

#[test]
fn adjacent_reflowed_statements_each_keep_their_own_line() {
    // Two statements that the formatter *both* wraps produce one diff gap
    // with no line-level anchor inside it. Splitting such a gap
    // proportionally lands the second statement's lines on the first, so
    // the gap is aligned by walking both sides' non-whitespace bytes.
    const SRC: &str = "\
class Profile frozen:
    ident: int
    name: str
    rating: int
    wins: int
    losses: int

def build(row: tuple[int, str, int, int, int], override_rating: int?) -> Profile:
    let rating: int = override_rating if override_rating is not None else int(row[2]) * 1000 + 7
    return Profile(ident=int(row[0]), name=str(row[1]), rating=rating, wins=int(row[3]), losses=int(row[4]))

def main() -> None:
    print(build((1, \"a\", 2, 3, 4), None))

if __name__ == \"__main__\":
    main()
";
    let tmp = tempfile::tempdir().unwrap();
    scaffold_formatted(tmp.path(), SRC);
    let (py, map) = build_and_load(tmp.path());

    assert_eq!(map.len(), py.len());
    assert_in_range(&map, SRC);

    if ruff_available() {
        assert!(
            py.iter().any(|l| l.trim_end() == "    return Profile(")
                && py.iter().any(|l| l.trim_end() == "    rating: int = ("),
            "this test needs ruff to wrap BOTH statements — with no anchor \
             line between them there is nothing for the patience diff to \
             latch onto; got:\n{}",
            py.join("\n")
        );
    }

    // Every line of the wrapped `return` — including its head, which the
    // proportional split used to attribute to the `rating` assignment
    // above it — must name the `return`'s own line.
    let return_ty = ty_line_of(SRC, "return Profile(");
    let rating_ty = ty_line_of(SRC, "let rating: int =");
    for needle in ["return Profile(", "wins=int(row[3])", "losses=int(row[4])"] {
        let got = mapped_lines_for(&py, &map, needle);
        assert!(
            !got.is_empty() && got.iter().all(|&v| v == return_ty),
            "`{needle}` must map to .ty line {return_ty}; got {got:?}"
        );
    }
    // …and the statement above keeps its own line rather than being
    // dragged forward by the `return`'s expansion.
    let rating_got = mapped_lines_for(&py, &map, "override_rating if override_rating is not None");
    assert!(
        !rating_got.is_empty() && rating_got.iter().all(|&v| v == rating_ty),
        "the wrapped `rating` assignment must map to .ty line {rating_ty}; got {rating_got:?}"
    );
}

#[test]
fn formatted_build_keeps_match_case_granularity() {
    // Task 1 and Task 2 have to compose: the emitter's per-clause offsets
    // survive the format-stage re-keying.
    const SRC: &str = "\
class Circle frozen:
    radius: float

class Square frozen:
    side: float

type Shape = Circle | Square

def scale(a: float, b: float, c: float, d: float, e: float, f: float) -> float:
    return a * b * c * d * e * f

def area(s: Shape, z: float) -> float:
    match s:
        case Circle(r):
            return scale(3.14159265358979, r, r, z, z, z) * scale(z, z, z, r, r, r) * z * z * z
        case Square(side):
            return side / z

def main() -> None:
    print(area(Square(side=3.0), 0.0))

if __name__ == \"__main__\":
    main()
";
    let tmp = tempfile::tempdir().unwrap();
    scaffold_formatted(tmp.path(), SRC);
    let (py, map) = build_and_load(tmp.path());

    assert_eq!(map.len(), py.len());
    assert_in_range(&map, SRC);
    if ruff_available() {
        // The first arm must be wrapped, so the `case` header below it sits
        // past a reflow — otherwise this test would only exercise Task 2.
        assert!(
            py.iter().any(|l| l.trim_end() == "            return ("),
            "expected ruff to wrap the first arm; got:\n{}",
            py.join("\n")
        );
    }
    assert_eq!(
        mapped_lines_for(&py, &map, "case Square(side):"),
        vec![ty_line_of(SRC, "case Square(side):")],
        "the `case` header must survive the formatting re-key"
    );
    assert_eq!(
        mapped_lines_for(&py, &map, "return side / z"),
        vec![ty_line_of(SRC, "return side / z")],
    );

    // And end to end: the division raises, and the frame must name the
    // `return side / z` line — not the `case` header above it.
    let Some(py_bin) = python() else {
        eprintln!("skipping the execution half: no Python 3.12+ interpreter on PATH");
        return;
    };
    let run = Command::new(&py_bin)
        .arg(tmp.path().join("build").join("main.py"))
        .output()
        .unwrap();
    let tb_path = tmp.path().join("traceback.txt");
    std::fs::write(&tb_path, run.stderr).unwrap();
    let traced = tyc()
        .arg("trace")
        .arg(&tb_path)
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let remapped = String::from_utf8_lossy(&traced.stdout).into_owned();
    let expected = ty_line_of(SRC, "return side / z");
    assert!(
        remapped.contains(&format!("main.ty\", line {expected},")),
        "the raising frame must land on .ty line {expected}; got:\n{remapped}"
    );
}
