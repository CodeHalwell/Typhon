//! End-to-end regression tests for the 2026-10-03 review's W3 workstream
//! (the `tyc-analyse` optimisers). An optimiser must never change what a
//! program prints, so each program is built twice — default and optimised —
//! and both builds must print exactly the hand-sequenced expected output on
//! CPython.

use std::path::Path;
use std::process::Command;

fn tyc() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tyc"));
    c.env("TYC_NO_SYNC", "1").env("TYC_NO_INTROSPECT", "1");
    c
}

/// A Python 3.13+ interpreter, or `None` to skip (a panic under
/// `TYC_REQUIRE_PYTHON=1`, as in the rest of the suite).
fn python() -> Option<String> {
    for candidate in ["python3.13", "python3"] {
        if let Ok(out) = Command::new(candidate)
            .args(["-c", "import sys; print(sys.version_info >= (3, 13))"])
            .output()
        {
            if out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "True" {
                return Some(candidate.to_owned());
            }
        }
    }
    assert!(
        std::env::var_os("TYC_REQUIRE_PYTHON").is_none(),
        "TYC_REQUIRE_PYTHON is set but no Python 3.13+ interpreter was found"
    );
    None
}

/// Scaffold a project. `strictness` is spliced into `[strictness]`; the
/// optimiser knobs are deliberately *not* pinned, so `-O` reaches them.
fn scaffold(dir: &Path, src: &str, strictness: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("typhon.toml"),
        format!(
            "[project]\nname = \"w3\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
             [python]\ntarget = \"3.13\"\n[emit]\nformat = false\n[strictness]\n{strictness}\n[env]\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("src").join("main.ty"), src).unwrap();
}

fn build_and_run(py: &str, src: &str, strictness: &str, optimise: bool) -> (String, String) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    scaffold(dir, src, strictness);
    let mut cmd = tyc();
    cmd.current_dir(dir).arg("build");
    if optimise {
        cmd.arg("-O");
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "tyc build failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let emitted = std::fs::read_to_string(dir.join("build").join("main.py")).unwrap();
    let run = Command::new(py)
        .current_dir(dir.join("build"))
        .arg("main.py")
        .output()
        .unwrap();
    let mut text = String::from_utf8_lossy(&run.stdout).into_owned();
    if !run.status.success() {
        let stderr = String::from_utf8_lossy(&run.stderr);
        text.push_str(&format!(
            "<exit {:?}: {}>",
            run.status.code(),
            stderr.lines().last().unwrap_or("")
        ));
    }
    (text, emitted)
}

/// Both the default build and the optimised build (`-O` plus `strictness`)
/// print exactly `expected`. Returns the optimised build's emitted Python.
fn assert_optimised_prints(src: &str, strictness: &str, expected: &str) -> Option<String> {
    let py = python()?;
    let (default_out, _) = build_and_run(&py, src, "", false);
    assert_eq!(default_out, expected, "default build output");
    let (opt_out, emitted) = build_and_run(&py, src, strictness, true);
    assert_eq!(
        opt_out, expected,
        "optimised build output differs; emitted:\n{emitted}"
    );
    Some(emitted)
}

// ── W3-04 / W3-05: memoisation ──────────────────────────────────────────────

#[test]
fn pure_returning_a_list_is_not_shared_under_o() {
    let src = "\
@pure
def window(n: int) -> list[int]:
    return list(range(n))

def main() -> None:
    let w = window(3)
    w.append(99)
    print(window(3))

main()
";
    assert_optimised_prints(src, "", "[0, 1, 2]\n");
}

#[test]
fn float_and_bool_arguments_do_not_share_a_cache_entry() {
    let src = "\
def show(x: float) -> str:
    return repr(x)

def label(n: int) -> str:
    return repr(n)

print(show(1.0), show(True), show(0.0), show(-0.0))
print(label(1), label(True))
";
    let emitted = assert_optimised_prints(src, "", "1.0 True 0.0 -0.0\n1 True\n");
    if let Some(py) = emitted {
        assert!(
            py.contains("@functools.lru_cache(maxsize=1024, typed=True)\ndef label("),
            "an int-keyed function is still cached, typed:\n{py}"
        );
        assert!(
            !py.contains("lru_cache(maxsize=1024, typed=True)\ndef show("),
            "a float-keyed function is not cached:\n{py}"
        );
    }
}

#[test]
fn closures_side_effecting_constructors_and_mutable_frozen_fields_are_not_cached() {
    let src = "\
from typing import Callable

class Box frozen:
    items: list[int]

class Tick:
    n: int

impl Tick:
    def __post_init__(self) -> None:
        print(\"tick\", self.n)

def make_counter(start: int) -> Callable[[], int]:
    mut n: int = start
    def step() -> int:
        nonlocal n
        n = n + 1
        return n
    return step

def boxed(n: int) -> Box:
    return Box(items=[n])

def ticked(n: int) -> int:
    let t = Tick(n=n)
    return t.n

def main() -> None:
    let a = make_counter(0)
    let b = make_counter(0)
    print(a(), a(), b())
    boxed(1).items.append(5)
    print(boxed(1).items)
    print(ticked(7), ticked(7))

main()
";
    assert_optimised_prints(src, "", "1 2 1\n[1]\ntick 7\ntick 7\n7 7\n");
}

// ── W3-07: auto-gather ──────────────────────────────────────────────────────

#[test]
fn a_gatherable_method_does_not_make_a_same_named_function_eligible() {
    // stress/round-2026-09-01/analyse/p_gather.ty
    let src = "\
import asyncio

class Client:
    n: int

impl Client:
    @gatherable
    async def fetch(self) -> int:
        return self.n

order: list[str] = []

async def fetch(tag: str, delay: float) -> str:
    await asyncio.sleep(delay)
    order.append(tag)
    return tag

async def main() -> None:
    let a: str = await fetch(\"a\", 0.05)
    let b: str = await fetch(\"b\", 0.0)
    print(order, a, b)

asyncio.run(main())
";
    assert_optimised_prints(src, "", "['a', 'b'] a b\n");
}

#[test]
fn a_folded_run_raises_the_original_exception_to_callers() {
    let src = "\
import asyncio

@gatherable
async def load_one(n: int) -> int:
    await asyncio.sleep(0)
    if n < 0:
        raise ValueError(\"negative\")
    return n

@gatherable
async def load_two(n: int) -> int:
    await asyncio.sleep(0.01)
    if n < -5:
        raise KeyError(\"very negative\")
    return n * 2

async def load(n: int) -> int:
    let a: int = await load_one(n)
    let b: int = await load_two(n)
    return a + b

async def main() -> None:
    print(await load(2))
    try:
        await load(-1)
    except ValueError as e:
        print(\"caught\", e)
    try:
        await load(-9)
    except ValueError as e:
        print(\"caught first\", e)

asyncio.run(main())
";
    let emitted = assert_optimised_prints(src, "", "6\ncaught negative\ncaught first negative\n");
    if let Some(py) = emitted {
        assert!(
            py.contains("asyncio.TaskGroup"),
            "the run still folds:\n{py}"
        );
    }
}
