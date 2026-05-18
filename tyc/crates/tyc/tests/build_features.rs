//! Integration tests for individual Typhon language features through the
//! full `tyc build` pipeline.
//!
//! Each test scaffolds a minimal project, builds it, and asserts on the
//! emitted Python.  Where the emitted output is expected to run, we also
//! pipe it through a real `python3` (any 3.13+) and check stdout/exit.
//! The Python interpreter must be discoverable on `PATH` as `python3.13`,
//! `python3.12`, or `python3`; tests that need it `skip` cleanly when no
//! supported interpreter is present.

use std::path::{Path, PathBuf};
use std::process::Command;

// ── helpers ───────────────────────────────────────────────────────────────────

fn tyc() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tyc"))
}

/// Locate a Python 3.12+ interpreter on `PATH`; returns `None` to skip.
fn python() -> Option<String> {
    for candidate in ["python3.13", "python3.12", "python3"] {
        let out = Command::new(candidate).arg("--version").output();
        if let Ok(out) = out {
            if out.status.success() {
                let v = String::from_utf8_lossy(&out.stdout);
                // Match `Python 3.12.x` or `Python 3.13.x` or later 3.x.
                if let Some(rest) = v.trim().strip_prefix("Python 3.") {
                    if let Some(minor) = rest.split('.').next() {
                        if let Ok(m) = minor.parse::<u32>() {
                            if m >= 12 {
                                return Some(candidate.to_owned());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

fn scaffold(dir: &Path, src_content: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("typhon.toml"),
        "[project]\nname = \"feat\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
         [python]\ntarget = \"3.13\"\n[emit]\nformat = false\n[strictness]\n[env]\n",
    )
    .unwrap();
    std::fs::write(dir.join("src").join("main.ty"), src_content).unwrap();
}

fn build(dir: &Path) {
    let status = tyc().arg("build").arg(dir).status().unwrap();
    assert!(status.success(), "tyc build failed for: {}", dir.display());
}

fn main_py(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("build").join("main.py")).unwrap()
}

/// Run the project's main.py with the resolved Python interpreter; assert
/// it exits successfully and return its stdout.  Returns `None` if no
/// suitable interpreter was found (test should treat this as a skip).
fn run_main(dir: &Path) -> Option<String> {
    let py = python()?;
    let out = Command::new(py)
        .arg(dir.join("build").join("main.py"))
        .output()
        .expect("python should spawn");
    assert!(
        out.status.success(),
        "main.py exited non-zero:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ── emitted-Python validity ──────────────────────────────────────────────────

#[test]
fn build_emits_valid_python_for_let_binding() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "let x: int = 42\n");
    build(tmp.path());
    let py = main_py(tmp.path());
    assert!(
        !py.lines().any(|l| l.trim_start().starts_with("let ")),
        "emitted Python must not contain `let` keyword; got:\n{py}"
    );
    assert!(py.contains("x: int = 42"), "got:\n{py}");
}

#[test]
fn build_emits_valid_python_for_mut_binding() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "mut counter: int = 0\n");
    build(tmp.path());
    let py = main_py(tmp.path());
    assert!(
        !py.lines().any(|l| l.trim_start().starts_with("mut ")),
        "emitted Python must not contain `mut` keyword; got:\n{py}"
    );
}

#[test]
fn build_emits_runnable_hello_world() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "print(\"hi\")\n");
    build(tmp.path());
    let Some(out) = run_main(tmp.path()) else {
        return;
    };
    assert_eq!(out.trim(), "hi");
}

#[test]
fn build_emits_runnable_let_then_print() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(
        tmp.path(),
        "let greeting: str = \"hello world\"\nprint(greeting)\n",
    );
    build(tmp.path());
    let Some(out) = run_main(tmp.path()) else {
        return;
    };
    assert_eq!(out.trim(), "hello world");
}

#[test]
fn build_emits_runnable_function_call() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(
        tmp.path(),
        "def double(x: int) -> int:\n    return x * 2\n\nprint(double(21))\n",
    );
    build(tmp.path());
    let Some(out) = run_main(tmp.path()) else {
        return;
    };
    assert_eq!(out.trim(), "42");
}

// ── class emission ───────────────────────────────────────────────────────────

#[test]
fn build_emits_dataclass_for_plain_class() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "class Point:\n    x: int\n    y: int\n");
    build(tmp.path());
    let py = main_py(tmp.path());
    assert!(py.contains("@dataclasses.dataclass"), "got:\n{py}");
    assert!(py.contains("class Point"), "got:\n{py}");
    assert!(py.contains("slots=True"), "got:\n{py}");
}

#[test]
fn build_emits_pydantic_model_for_model_keyword() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "model User:\n    name: str\n    age: int\n");
    build(tmp.path());
    let py = main_py(tmp.path());
    assert!(
        py.contains("BaseModel") || py.contains("pydantic"),
        "model keyword should produce a Pydantic class; got:\n{py}"
    );
}

#[test]
fn build_class_with_methods_runs() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(
        tmp.path(),
        "class Greeter:\n    name: str\n    def hello(self) -> str:\n        return \"hi \" + self.name\n\nprint(Greeter(\"world\").hello())\n",
    );
    build(tmp.path());
    let Some(out) = run_main(tmp.path()) else {
        return;
    };
    assert_eq!(out.trim(), "hi world");
}

// ── interface / Protocol ────────────────────────────────────────────────────

#[test]
fn build_emits_protocol_for_interface() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(
        tmp.path(),
        "interface Greeter:\n    def greet(self) -> str:\n        ...\n",
    );
    build(tmp.path());
    let py = main_py(tmp.path());
    assert!(
        py.contains("Protocol"),
        "interface should lower to typing.Protocol; got:\n{py}"
    );
}

// ── Result / Ok / Err runtime wiring ────────────────────────────────────────

#[test]
fn build_emits_typhon_runtime_package_when_ok_used() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "def f() -> Ok[int]:\n    return Ok(1)\n");
    build(tmp.path());
    let pkg = tmp.path().join("build").join("typhon_runtime");
    assert!(pkg.join("__init__.py").exists(), "__init__.py missing");
    assert!(pkg.join("tasks.py").exists(), "tasks.py missing");
    assert!(pkg.join("lazy.py").exists(), "lazy.py missing");
    assert!(
        pkg.join("stdlib.py").exists(),
        "stdlib.py should be emitted alongside the package"
    );
    assert!(
        pkg.join("result.py").exists(),
        "result.py should be emitted alongside the package"
    );
}

#[test]
fn build_omits_runtime_when_unused() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "print(\"no runtime needed\")\n");
    build(tmp.path());
    assert!(
        !tmp.path().join("build").join("typhon_runtime").exists(),
        "typhon_runtime/ must not be emitted for plain Python"
    );
}

#[test]
fn build_ok_result_runs_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(
        tmp.path(),
        "def f() -> Ok[int]:\n    return Ok(1)\n\nprint(f().value)\n",
    );
    build(tmp.path());
    let Some(out) = run_main(tmp.path()) else {
        return;
    };
    assert_eq!(out.trim(), "1");
}

// ── source map sidecars ──────────────────────────────────────────────────────

#[test]
fn build_writes_py_map_sidecar() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "let x: int = 1\n");
    build(tmp.path());
    let map = tmp.path().join("build").join("main.py.map");
    assert!(map.exists(), "main.py.map sidecar should be written");
    let body = std::fs::read_to_string(&map).unwrap();
    assert!(body.contains("\"source\""), "map should include source key");
    assert!(body.contains("\"lines\""), "v2 map should include lines");
}

// ── multi-file projects ──────────────────────────────────────────────────────

#[test]
fn build_processes_nested_src_files() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(src.join("pkg")).unwrap();
    std::fs::write(
        tmp.path().join("typhon.toml"),
        "[project]\nname=\"m\"\nversion=\"0.1.0\"\nsrc=\"src\"\nout=\"build\"\n\
         [python]\ntarget=\"3.13\"\n[emit]\nformat=false\n[strictness]\n[env]\n",
    )
    .unwrap();
    std::fs::write(src.join("main.ty"), "let a: int = 1\n").unwrap();
    std::fs::write(src.join("pkg").join("util.ty"), "let b: int = 2\n").unwrap();

    build(tmp.path());

    assert!(tmp.path().join("build").join("main.py").exists());
    assert!(
        tmp.path()
            .join("build")
            .join("pkg")
            .join("util.py")
            .exists(),
        "nested .ty should produce nested .py"
    );
}

#[test]
fn build_out_dir_override_is_respected() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "let x: int = 1\n");
    let custom = tmp.path().join("custom-out");
    let status = tyc()
        .arg("build")
        .arg(tmp.path())
        .arg("-o")
        .arg(&custom)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(custom.join("main.py").exists(), "should use --out override");
    assert!(
        !tmp.path().join("build").exists(),
        "default build/ should not be created when --out is given"
    );
}

// ── check command — broader coverage ────────────────────────────────────────

#[test]
fn check_reports_unknown_name() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("u.ty"), "let y: int = unknown_name\n").unwrap();
    let out = tyc().arg("check").arg(tmp.path()).output().unwrap();
    assert!(!out.status.success(), "check must fail on unknown name");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let combined = format!("{stderr}{stdout}");
    assert!(
        combined.to_lowercase().contains("unknown")
            || combined.to_lowercase().contains("find")
            || combined.to_lowercase().contains("scope"),
        "expected an unknown-name diagnostic; got:\n{combined}"
    );
}

#[test]
fn check_passes_on_clean_function() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("f.ty"),
        "def add(a: int, b: int) -> int:\n    return a + b\n",
    )
    .unwrap();
    assert!(tyc()
        .arg("check")
        .arg(tmp.path())
        .status()
        .unwrap()
        .success());
}

#[test]
fn check_fails_when_none_assigned_to_non_optional() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("n.ty"), "let x: int = None\n").unwrap();
    let status = tyc().arg("check").arg(tmp.path()).status().unwrap();
    assert!(!status.success(), "must fail: None into non-optional");
}

// ── format command ──────────────────────────────────────────────────────────

#[test]
fn fmt_preserves_let_keyword() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("a.ty");
    std::fs::write(&p, "let x: int = 1\n").unwrap();
    assert!(tyc().arg("fmt").arg(&p).status().unwrap().success());
    let content = std::fs::read_to_string(&p).unwrap();
    assert!(
        content.contains("let x"),
        "fmt must round-trip `let`; got:\n{content}"
    );
}

#[test]
fn fmt_collapses_trailing_whitespace() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("ws.ty");
    std::fs::write(&p, "let x: int = 1   \n").unwrap();
    assert!(tyc().arg("fmt").arg(&p).status().unwrap().success());
    let content = std::fs::read_to_string(&p).unwrap();
    assert!(
        !content.lines().any(|l| l.ends_with(' ')),
        "trailing spaces should be stripped; got:\n{content}"
    );
}

// ── CLI surface tests ────────────────────────────────────────────────────────

#[test]
fn tyc_help_lists_all_subcommands() {
    let out = tyc().arg("--help").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for cmd in [
        "build", "check", "fmt", "init", "lsp", "trace", "profile", "migrate", "ty", "repl",
        "debug",
    ] {
        assert!(
            text.contains(cmd),
            "subcommand `{cmd}` missing from help:\n{text}"
        );
    }
}

#[test]
fn tyc_version_returns_zero() {
    let status = tyc().arg("--version").status().unwrap();
    assert!(status.success());
}

#[test]
fn tyc_unknown_subcommand_fails() {
    let status = tyc().arg("nonsense").status().unwrap();
    assert!(!status.success(), "unknown subcommand should exit non-zero");
}

// ── trace command — extra cases ──────────────────────────────────────────────

#[test]
fn trace_stdin_passthrough_with_no_frames() {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = tyc()
        .arg("trace")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"Hello, no frames here\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Hello, no frames here"),
        "trace must passthrough non-frame stdin"
    );
}

#[test]
fn trace_handles_traceback_with_no_map() {
    let dir = tempfile::tempdir().unwrap();
    let tb = dir.path().join("tb.txt");
    std::fs::write(
        &tb,
        "Traceback (most recent call last):\n  File \"/tmp/imaginary.py\", line 7, in fn\nValueError: x\n",
    )
    .unwrap();
    let out = tyc().arg("trace").arg(&tb).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // No .py.map adjacent → frame passes through unchanged.
    assert!(
        stdout.contains("/tmp/imaginary.py"),
        "unmapped frame should pass through; got:\n{stdout}"
    );
}

// ── repl command — non-interactive smoke test ───────────────────────────────

#[test]
fn repl_evaluates_simple_block_and_exits_clean() {
    let Some(py) = python() else { return };
    let mut child = tyc()
        .arg("repl")
        .arg("--python")
        .arg(&py)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"let x: int = 7\nprint(x + 1)\n:quit\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "repl should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("8"),
        "repl should print `8`; got:\n{stdout}"
    );
}

#[test]
fn repl_rejects_type_error_block_keeps_session() {
    let Some(py) = python() else { return };
    let mut child = tyc()
        .arg("repl")
        .arg("--python")
        .arg(&py)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"let x: int = 1\nlet y: int = \"oops\"\nprint(x)\n:quit\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "repl should not crash on type error");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // First block ok → 1 isn't auto-printed but x is still in scope for `print(x)`.
    assert!(
        stdout.contains('1'),
        "session state should survive; got:\n{stdout}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.to_lowercase().contains("error")
            || stdout.to_lowercase().contains("error")
            || stdout.to_lowercase().contains("mismatch"),
        "error should be reported; stdout=\n{stdout}\nstderr=\n{stderr}"
    );
}

#[test]
fn repl_load_flag_preloads_file() {
    let Some(py) = python() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let load = tmp.path().join("init.ty");
    std::fs::write(&load, "print(\"loaded\")\n").unwrap();
    let mut child = tyc()
        .arg("repl")
        .arg("--python")
        .arg(&py)
        .arg("--load")
        .arg(&load)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.as_mut().unwrap().write_all(b":quit\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("loaded"),
        "preloaded file should run; got:\n{stdout}"
    );
}

#[test]
fn repl_reset_clears_session() {
    let Some(py) = python() else { return };
    let mut child = tyc()
        .arg("repl")
        .arg("--python")
        .arg(&py)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"let x: int = 99\n:reset\nprint(\"after-reset\")\n:quit\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("after-reset"),
        "post-reset block should run; got:\n{stdout}"
    );
}

// ── debug command — surface tests ───────────────────────────────────────────

#[test]
fn debug_help_documents_flags() {
    let out = tyc().args(["debug", "--help"]).output().unwrap();
    assert!(out.status.success(), "debug --help should succeed");
    let text = String::from_utf8_lossy(&out.stdout);
    for flag in ["--python", "--debugger", "--entry", "--no-build"] {
        assert!(
            text.contains(flag),
            "debug --help missing flag `{flag}`:\n{text}"
        );
    }
}

#[test]
fn debug_no_build_with_missing_entry_fails() {
    let tmp = tempfile::tempdir().unwrap();
    // No build directory exists; --no-build skips the build, so debug
    // should fail to find the entry point.
    let status = tyc()
        .args(["debug", "--no-build", "--entry", "main.py"])
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "debug --no-build with missing entry must fail"
    );
}

// ── stdlib runtime — content checks ─────────────────────────────────────────

#[test]
fn build_emits_stdlib_with_parse_int() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "def f() -> Ok[int]:\n    return Ok(1)\n");
    build(tmp.path());
    let stdlib =
        std::fs::read_to_string(tmp.path().join("build/typhon_runtime/stdlib.py")).unwrap();
    for name in [
        "parse_int",
        "parse_float",
        "chunked",
        "flatten",
        "unique",
        "group_by",
        "strip_prefix",
        "strip_suffix",
        "split_once",
        "retry",
    ] {
        assert!(
            stdlib.contains(&format!("def {name}(")),
            "stdlib should define `{name}`; not found in:\n{stdlib}"
        );
    }
}

#[test]
fn build_emits_result_helpers_module() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "def f() -> Ok[int]:\n    return Ok(1)\n");
    build(tmp.path());
    let result =
        std::fs::read_to_string(tmp.path().join("build/typhon_runtime/result.py")).unwrap();
    for name in [
        "is_ok",
        "is_err",
        "map",
        "map_err",
        "and_then",
        "or_else",
        "unwrap",
        "unwrap_or",
        "unwrap_or_else",
    ] {
        assert!(
            result.contains(&format!("def {name}(")),
            "result.py should define `{name}`; not found in:\n{result}"
        );
    }
}

#[test]
fn stdlib_python_executes_correctly() {
    let Some(py) = python() else { return };
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "def f() -> Ok[int]:\n    return Ok(1)\n");
    build(tmp.path());
    let build_dir = tmp.path().join("build");
    let script = "\
import sys
sys.path.insert(0, %r)
from typhon_runtime import stdlib
from typhon_runtime.result import is_ok, is_err, unwrap, unwrap_or
ok = stdlib.parse_int('100')
bad = stdlib.parse_int('not-a-number')
assert is_ok(ok), ok
assert is_err(bad), bad
assert unwrap(ok) == 100
assert unwrap_or(bad, -1) == -1
assert list(stdlib.chunked([1, 2, 3, 4, 5], 2)) == [[1, 2], [3, 4], [5]]
assert list(stdlib.unique([1, 1, 2, 3, 2])) == [1, 2, 3]
assert stdlib.strip_prefix('hello world', 'hello ') == 'world'
assert stdlib.strip_suffix('foo.py', '.py') == 'foo'
assert stdlib.split_once('a=b=c', '=') == ('a', 'b=c')
assert stdlib.group_by(['ant', 'apple', 'bee'], lambda s: s[0]) == {'a': ['ant', 'apple'], 'b': ['bee']}
print('OK')
";
    let formatted = script.replacen("%r", &format!("{:?}", build_dir.display().to_string()), 1);
    let out = Command::new(py).arg("-c").arg(&formatted).output().unwrap();
    assert!(
        out.status.success(),
        "stdlib smoke test failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("OK"));
}

#[test]
fn stdlib_retry_returns_ok_after_success() {
    let Some(py) = python() else { return };
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "def f() -> Ok[int]:\n    return Ok(1)\n");
    build(tmp.path());
    let build_dir = tmp.path().join("build");
    let script = format!(
        "\
import sys
sys.path.insert(0, {build_dir:?})
from typhon_runtime import stdlib
from typhon_runtime.result import is_ok, unwrap
called = []
def succeed():
    called.append(None)
    return 7
r = stdlib.retry(succeed, attempts=3, backoff=0.001)
assert is_ok(r), r
assert unwrap(r) == 7
assert len(called) == 1
print('OK')
",
        build_dir = build_dir.display().to_string()
    );
    let out = Command::new(py).arg("-c").arg(&script).output().unwrap();
    assert!(
        out.status.success(),
        "retry test failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn stdlib_retry_returns_err_after_exhausting_attempts() {
    let Some(py) = python() else { return };
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "def f() -> Ok[int]:\n    return Ok(1)\n");
    build(tmp.path());
    let build_dir = tmp.path().join("build");
    let script = format!(
        "\
import sys
sys.path.insert(0, {build_dir:?})
from typhon_runtime import stdlib
from typhon_runtime.result import is_err
called = []
def always_fail():
    called.append(None)
    raise RuntimeError('nope')
r = stdlib.retry(always_fail, attempts=3, backoff=0.001)
assert is_err(r), r
assert len(called) == 3, called
print('OK')
",
        build_dir = build_dir.display().to_string()
    );
    let out = Command::new(py).arg("-c").arg(&script).output().unwrap();
    assert!(
        out.status.success(),
        "retry-exhaust test failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn typhon_runtime_init_reexports_result_and_stdlib() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "def f() -> Ok[int]:\n    return Ok(1)\n");
    build(tmp.path());
    let init =
        std::fs::read_to_string(tmp.path().join("build/typhon_runtime/__init__.py")).unwrap();
    assert!(init.contains("stdlib"), "stdlib should be re-exported");
    assert!(init.contains("result"), "result should be re-exported");
}

// ── init command — argument coverage ────────────────────────────────────────

#[test]
fn init_with_explicit_name_uses_given_name() {
    let tmp = tempfile::tempdir().unwrap();
    let sub = tmp.path().join("nested");
    std::fs::create_dir(&sub).unwrap();
    let status = tyc()
        .args(["init", "specific", "--dir"])
        .arg(&sub)
        .status()
        .unwrap();
    assert!(status.success());
    let toml = std::fs::read_to_string(sub.join("typhon.toml")).unwrap();
    assert!(
        toml.contains("specific"),
        "explicit name should win over dir name; got:\n{toml}"
    );
}

#[test]
fn init_main_ty_compiles_via_check() {
    let tmp = tempfile::tempdir().unwrap();
    tyc()
        .args(["init", "demo", "--dir"])
        .arg(tmp.path())
        .status()
        .unwrap();
    let status = tyc().arg("check").arg(tmp.path()).status().unwrap();
    assert!(
        status.success(),
        "scaffolded project should type-check out of the box"
    );
}

#[test]
fn init_main_ty_builds_to_valid_python() {
    let Some(py) = python() else { return };
    let tmp = tempfile::tempdir().unwrap();
    tyc()
        .args(["init", "demo", "--dir"])
        .arg(tmp.path())
        .status()
        .unwrap();
    let status = tyc().arg("build").arg(tmp.path()).status().unwrap();
    assert!(status.success(), "scaffolded project should build");
    let out = Command::new(py)
        .arg(tmp.path().join("build").join("main.py"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "init main.py should be runnable; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ── working directory of build command ──────────────────────────────────────

#[test]
fn build_writes_relative_to_typhon_toml_not_cwd() {
    // tyc build should locate typhon.toml and emit relative to it,
    // even when invoked from elsewhere with an explicit path argument.
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "let x: int = 1\n");

    // Run from a totally different cwd.
    let other = tempfile::tempdir().unwrap();
    let status = tyc()
        .current_dir(other.path())
        .arg("build")
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(status.success(), "tyc build must work from any cwd");
    assert!(
        tmp.path().join("build").join("main.py").exists(),
        "output must land relative to the project, not the cwd"
    );
    assert!(
        !other.path().join("build").exists(),
        "no build dir should be created in the cwd"
    );
}

#[test]
fn check_accepts_default_dot_path() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("only.ty"), "let x: int = 1\n").unwrap();
    let status = tyc().current_dir(tmp.path()).arg("check").status().unwrap();
    assert!(status.success(), "tyc check with no path should scan cwd");
}

// ── format --check exit codes ───────────────────────────────────────────────

#[test]
fn fmt_check_passes_on_clean_directory() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.ty"), "let x: int = 1\n").unwrap();
    std::fs::write(tmp.path().join("b.ty"), "let y: int = 2\n").unwrap();
    let status = tyc()
        .args(["fmt", "--check"])
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(status.success(), "fmt --check must pass on clean dir");
}

#[test]
fn fmt_check_fails_if_any_file_needs_changes() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.ty"), "let x: int = 1\n").unwrap();
    std::fs::write(tmp.path().join("dirty.ty"), "def f():\n\tpass\n").unwrap();
    let status = tyc()
        .args(["fmt", "--check"])
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "fmt --check must fail when any file needs changes"
    );
}

// ── ensuring CARGO_BIN_EXE is set ───────────────────────────────────────────

#[test]
fn cargo_bin_exe_path_is_set() {
    let path = PathBuf::from(env!("CARGO_BIN_EXE_tyc"));
    assert!(
        path.exists(),
        "CARGO_BIN_EXE_tyc should point at a real file"
    );
    assert!(path.is_file());
}
