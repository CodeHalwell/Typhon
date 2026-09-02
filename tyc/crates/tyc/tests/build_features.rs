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
///
/// **`TYC_REQUIRE_PYTHON=1` turns the skip into a panic.** Every test that
/// executes emitted Python was silently no-opping wherever `python3` was
/// absent — including in CI, whose test job never installed Python — so the
/// entire "does the output actually run" tier of the suite was green without
/// ever running. A skip that nothing can observe is indistinguishable from a
/// pass, so CI sets this variable and a missing interpreter fails the job.
fn python() -> Option<String> {
    let found = python_inner();
    if found.is_none() && std::env::var_os("TYC_REQUIRE_PYTHON").is_some() {
        panic!(
            "TYC_REQUIRE_PYTHON is set but no Python 3.12+ interpreter was found on PATH. \
             These tests execute the emitted Python; skipping them would report a pass \
             for a tier of the suite that never ran."
        );
    }
    found
}

fn python_inner() -> Option<String> {
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

/// Scaffold a project that opts into `[strictness] auto-parallel = true`
/// plus free-threaded Python so the loop-parallelisation rewrite fires
/// against the emitted code.
fn scaffold_parallel(dir: &Path, src_content: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("typhon.toml"),
        "[project]\nname = \"feat\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
         [python]\ntarget = \"3.13\"\nfree-threaded = true\n\
         [emit]\nformat = false\n\
         [strictness]\nauto-parallel = true\n[env]\n",
    )
    .unwrap();
    std::fs::write(dir.join("src").join("main.ty"), src_content).unwrap();
}

/// Scaffold a project with an explicit `[python] target`, keeping
/// `[emit] format = false` so the emitted Python is deterministic
/// (no ruff line-shifting) for byte-level assertions.
fn scaffold_target(dir: &Path, target: &str, src_content: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("typhon.toml"),
        format!(
            "[project]\nname = \"feat\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
             [python]\ntarget = \"{target}\"\n[emit]\nformat = false\n[strictness]\n[env]\n",
        ),
    )
    .unwrap();
    std::fs::write(dir.join("src").join("main.ty"), src_content).unwrap();
}

/// Run `tyc build` only when a usable Python interpreter is present.
///
/// Tests that subsequently execute the emitted Python use this gate so a
/// machine without Python skips the build step too rather than running it
/// only to drop the artifact on the floor.  Returns `None` when no Python
/// 3.12+ interpreter could be discovered.
fn build_if_python(dir: &Path) -> Option<String> {
    let py = python()?;
    build(dir);
    Some(py)
}

/// Combined `build_if_python` + `run_main`: skips upfront when Python is
/// absent, otherwise builds and runs `main.py`. Returns the script's stdout.
fn build_and_run_main(dir: &Path) -> Option<String> {
    let _py = build_if_python(dir)?;
    run_main(dir)
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
    let Some(out) = build_and_run_main(tmp.path()) else {
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
    let Some(out) = build_and_run_main(tmp.path()) else {
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
    let Some(out) = build_and_run_main(tmp.path()) else {
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
    let Some(out) = build_and_run_main(tmp.path()) else {
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
    let Some(out) = build_and_run_main(tmp.path()) else {
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
    let map = tmp
        .path()
        .join("build")
        .join(".sourcemaps")
        .join("main.py.map");
    assert!(
        map.exists(),
        "main.py.map sidecar should be written under .sourcemaps/"
    );
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
    // Asserting the bare subcommand name is too weak — e.g. `"ty"` is also
    // a prefix of `"tyc"`, `"Typhon"`, etc., so the assertion would pass
    // even if the subcommand were dropped. Match each command at the
    // start of its line (clap renders subcommands as `  <name>  <desc>`).
    for cmd in [
        "build", "check", "fmt", "init", "lsp", "trace", "profile", "migrate", "ty", "repl",
        "debug",
    ] {
        let expected = format!("  {cmd} ");
        let expected_indent4 = format!("    {cmd} ");
        assert!(
            text.lines()
                .any(|l| l.starts_with(&expected) || l.starts_with(&expected_indent4)),
            "subcommand `{cmd}` missing from help (looked for line starting with `{expected}`):\n{text}"
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

// ── extend BUILTIN — extension methods on built-ins ─────────────────────────

#[test]
fn build_accepts_extend_str_and_emits_free_function() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(
        tmp.path(),
        "extend str:\n    def shout(self) -> str:\n        return self.upper()\n",
    );
    build(tmp.path());
    let py = main_py(tmp.path());
    assert!(
        py.contains("def __typhon_ext_str__shout__"),
        "extend str: should lower to a free function; got:\n{py}"
    );
    assert!(
        !py.contains("class __typhon_builtin_ext_str"),
        "the sentinel class must be removed; got:\n{py}"
    );
}

#[test]
fn build_rewrites_str_extension_call_site() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(
        tmp.path(),
        "extend str:\n    def shout(self) -> str:\n        return self.upper()\n\n\
         let greeting: str = \"hi\"\nprint(greeting.shout())\n",
    );
    build(tmp.path());
    let py = main_py(tmp.path());
    assert!(
        py.contains("__typhon_ext_str__shout__(greeting)"),
        "call site must be rewritten; got:\n{py}"
    );
    if let Some(out) = run_main(tmp.path()) {
        assert_eq!(out.trim(), "HI");
    }
}

#[test]
fn build_cross_module_extend_str_rewrites_consumer_call_site() {
    // Reproducer for #202: `extend str:` in a dependency module should
    // be visible to a consumer module's call site.
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        tmp.path().join("typhon.toml"),
        "[project]\nname=\"ext\"\nversion=\"0.1.0\"\nsrc=\"src\"\nout=\"build\"\n\
         [python]\ntarget=\"3.13\"\n[emit]\nformat=false\n[strictness]\n[env]\n",
    )
    .unwrap();
    // The provider module declares the extension.
    std::fs::write(
        src.join("textutil.ty"),
        "extend str:\n    def slug(self) -> str:\n        return self.lower().replace(\" \", \"-\")\n",
    )
    .unwrap();
    // The consumer module imports from the provider and uses the method.
    std::fs::write(
        src.join("main.ty"),
        "from textutil import *\n\n\
         let title: str = \"Hello World\"\n\
         let s: str = title.slug()\n\
         print(s)\n",
    )
    .unwrap();

    build(tmp.path());
    let py = std::fs::read_to_string(tmp.path().join("build").join("main.py")).unwrap();
    // The consumer's call site must be rewritten to the free-function form.
    assert!(
        py.contains("__typhon_ext_str__slug__(title)"),
        "cross-module extension call site must be rewritten; got:\n{py}"
    );
    // An explicit import for the free function must be injected.
    assert!(
        py.contains("from textutil import __typhon_ext_str__slug__"),
        "cross-module extension must inject import; got:\n{py}"
    );
    // Runtime: the emitted Python must actually work.
    if let Some(out) = run_main(tmp.path()) {
        assert_eq!(out.trim(), "hello-world");
    }
}

#[test]
fn check_cross_module_extend_str_no_attribute_not_found() {
    // Verify that `tyc check` does NOT fire `attribute_not_found` for a
    // cross-module builtin extension method.
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        tmp.path().join("typhon.toml"),
        "[project]\nname=\"ext\"\nversion=\"0.1.0\"\nsrc=\"src\"\nout=\"build\"\n\
         [python]\ntarget=\"3.13\"\n[emit]\nformat=false\n[strictness]\n[env]\n",
    )
    .unwrap();
    std::fs::write(
        src.join("textutil.ty"),
        "extend str:\n    def slug(self) -> str:\n        return self.lower().replace(\" \", \"-\")\n",
    )
    .unwrap();
    std::fs::write(
        src.join("main.ty"),
        "from textutil import *\n\n\
         let title: str = \"Hello World\"\n\
         let s: str = title.slug()\n\
         print(s)\n",
    )
    .unwrap();

    let out = tyc()
        .arg("check")
        .arg(tmp.path().join("src"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("attribute_not_found"),
        "cross-module builtin extension must NOT trigger attribute_not_found; stderr:\n{stderr}"
    );
    assert!(
        out.status.success(),
        "tyc check should pass for cross-module extension; stderr:\n{stderr}"
    );
}

// ── auto-parallel comprehensions ────────────────────────────────────────────

#[test]
fn build_rewrites_pure_listcomp_under_auto_parallel() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold_parallel(
        tmp.path(),
        "@pure\ndef square(n: int) -> int:\n    return n * n\n\n\
         xs: list[int] = [1, 2, 3]\nys: list[int] = [square(x) for x in xs]\n",
    );
    build(tmp.path());
    let py = main_py(tmp.path());
    assert!(
        py.contains("typhon_runtime.parallel.map_pure"),
        "auto-parallel should rewrite the comprehension; got:\n{py}"
    );
    let pkg = tmp.path().join("build/typhon_runtime/parallel.py");
    assert!(pkg.exists(), "parallel.py helper must be emitted");
}

#[test]
fn build_leaves_impure_comprehension_alone_under_auto_parallel() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold_parallel(
        tmp.path(),
        "def io_call(n: int) -> int:\n    print(n)\n    return n\n\n\
         xs: list[int] = [1, 2, 3]\nys: list[int] = [io_call(x) for x in xs]\n",
    );
    build(tmp.path());
    let py = main_py(tmp.path());
    assert!(
        !py.contains("typhon_runtime.parallel.map_pure"),
        "impure callee must not trigger the rewrite; got:\n{py}"
    );
}

#[test]
fn build_skips_rewrite_when_auto_parallel_off() {
    // Default scaffold has auto-parallel disabled.
    let tmp = tempfile::tempdir().unwrap();
    scaffold(
        tmp.path(),
        "@pure\ndef square(n: int) -> int:\n    return n * n\n\n\
         xs: list[int] = [1, 2, 3]\nys: list[int] = [square(x) for x in xs]\n",
    );
    build(tmp.path());
    let py = main_py(tmp.path());
    assert!(
        !py.contains("typhon_runtime.parallel.map_pure"),
        "rewrite must require the auto-parallel opt-in; got:\n{py}"
    );
}

// ── auto-parallel widened shapes + reductions ───────────────────────────────

/// Scaffold a project with `free-threaded = true`, `auto-parallel = true`,
/// `auto-parallel-reductions = true`, and the given `parallel-backend`.
fn scaffold_parallel_full(dir: &Path, backend: &str, src_content: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("typhon.toml"),
        format!(
            "[project]\nname = \"feat\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
             [python]\ntarget = \"3.13\"\nfree-threaded = true\n\
             [emit]\nformat = false\n\
             [strictness]\nauto-parallel = true\nauto-parallel-reductions = true\n\
             parallel-backend = \"{backend}\"\n[env]\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("src").join("main.ty"), src_content).unwrap();
}

#[test]
fn build_rewrites_widened_comprehension_shapes() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold_parallel(
        tmp.path(),
        "@pure\ndef sq(n: int) -> int:\n    return n * n\n\n\
         def run(xs: list[int]) -> list[int]:\n    let k: int = 2\n    \
         return [sq(x) for x in xs if x > 0]\n",
    );
    build(tmp.path());
    let py = main_py(tmp.path());
    assert!(
        py.contains("typhon_runtime.parallel.map_pure"),
        "filtered comprehension should be parallelised; got:\n{py}"
    );
    assert!(
        py.contains("for x in xs if x > 0"),
        "filter should run sequentially in the map source; got:\n{py}"
    );
}

#[test]
fn build_rewrites_int_reduction_under_auto_parallel_reductions() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold_parallel_full(
        tmp.path(),
        "threads",
        "def run(xs: list[int]) -> int:\n    mut total: int = 0\n    \
         for x in xs:\n        total += x * x\n    return total\n",
    );
    build(tmp.path());
    let py = main_py(tmp.path());
    assert!(
        py.contains("total += sum(typhon_runtime.parallel.map_pure"),
        "int reduction should be folded into sum(map_pure(...)); got:\n{py}"
    );
}

#[test]
fn build_leaves_float_reduction_alone() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold_parallel_full(
        tmp.path(),
        "threads",
        "def run(xs: list[float]) -> float:\n    mut total: float = 0.0\n    \
         for x in xs:\n        total += x\n    return total\n",
    );
    build(tmp.path());
    let py = main_py(tmp.path());
    assert!(
        !py.contains("map_pure"),
        "float reduction must NOT be parallelised; got:\n{py}"
    );
    // The original sequential loop is preserved.
    assert!(
        py.contains("total += x"),
        "float loop should be untouched; got:\n{py}"
    );
}

// ── interpreters backend ────────────────────────────────────────────────────

#[test]
fn parallel_backend_threads_bakes_backend_constant_and_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold_parallel_full(
        tmp.path(),
        "threads",
        "@pure\ndef sq(n: int) -> int:\n    return n * n\n\n\
         xs: list[int] = [1, 2, 3]\nys: list[int] = [sq(x) for x in xs]\n",
    );
    build(tmp.path());
    let parallel_py =
        std::fs::read_to_string(tmp.path().join("build/typhon_runtime/parallel.py")).unwrap();
    assert!(
        parallel_py.contains("_BACKEND = \"threads\""),
        "threads backend constant missing:\n{parallel_py}"
    );
    // The fallback chain is present for both settings; only the constant differs.
    assert!(
        parallel_py.contains("InterpreterPoolExecutor")
            && parallel_py.contains("ThreadPoolExecutor")
            && parallel_py.contains("_try_interpreters"),
        "fallback chain missing:\n{parallel_py}"
    );
}

#[test]
fn parallel_backend_interpreters_bakes_backend_constant_and_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold_parallel_full(
        tmp.path(),
        "interpreters",
        "@pure\ndef sq(n: int) -> int:\n    return n * n\n\n\
         xs: list[int] = [1, 2, 3]\nys: list[int] = [sq(x) for x in xs]\n",
    );
    build(tmp.path());
    let parallel_py =
        std::fs::read_to_string(tmp.path().join("build/typhon_runtime/parallel.py")).unwrap();
    assert!(
        parallel_py.contains("_BACKEND = \"interpreters\""),
        "interpreters backend constant missing:\n{parallel_py}"
    );
    assert!(
        parallel_py.contains("from concurrent.futures import InterpreterPoolExecutor")
            && parallel_py.contains("except (ImportError, AttributeError)")
            && parallel_py.contains("except (TypeError, pickle.PicklingError)")
            && parallel_py.contains("with ThreadPoolExecutor(max_workers=max_workers) as pool"),
        "interpreters fallback chain missing:\n{parallel_py}"
    );
    // The pre-flight serialisation probe: an unpicklable callable (every
    // lambda the rewrites emit) must be rejected *before* a pool is created,
    // whatever exception pickling raises — PicklingError / AttributeError,
    // not just the TypeError the pool-block catch covers.
    assert!(
        parallel_py.contains("pickle.dumps(fn)") && parallel_py.contains("except Exception:"),
        "pre-flight pickle probe missing:\n{parallel_py}"
    );
}

#[test]
fn reduction_and_comprehension_rewrites_match_sequential_output() {
    // Execution-equivalence: the parallel build and the sequential (knobs-off)
    // build must produce identical stdout. On a GIL / 3.13 build the map_pure
    // helper serialises the workers, so the point is correctness, not speed.
    let Some(_py) = python() else {
        return; // skip without an interpreter
    };
    let program = "\
@pure
def sq(n: int) -> int:
    return n * n

def main() -> None:
    let xs: list[int] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
    let squared: list[int] = [sq(x) for x in xs if x % 2 == 0]
    mut total: int = 0
    for x in xs:
        total += sq(x)
    print(squared)
    print(total)

if __name__ == \"__main__\":
    main()
";
    // Parallel build (auto-parallel + reductions + free-threaded).
    let par = tempfile::tempdir().unwrap();
    scaffold_parallel_full(par.path(), "threads", program);
    build(par.path());
    let par_out = run_main(par.path()).expect("parallel build should run");

    // Sequential build (default scaffold: all parallel knobs off).
    let seq = tempfile::tempdir().unwrap();
    scaffold(seq.path(), program);
    build(seq.path());
    let seq_out = run_main(seq.path()).expect("sequential build should run");

    assert_eq!(
        par_out, seq_out,
        "parallel and sequential builds must produce identical output"
    );
    // Sanity: the parallel build actually applied both rewrites.
    let par_py = main_py(par.path());
    assert!(
        par_py.contains("map_pure"),
        "comprehension rewrite missing:\n{par_py}"
    );
    assert!(
        par_py.contains("total += sum(typhon_runtime.parallel.map_pure"),
        "reduction rewrite missing:\n{par_py}"
    );
}

#[test]
fn interpreters_backend_falls_back_and_runs_on_gil_build() {
    // With backend=interpreters on a 3.13 interpreter (no InterpreterPoolExecutor),
    // map_pure must fall back to the thread pool and still produce correct output.
    let Some(_py) = python() else {
        return;
    };
    let program = "\
@pure
def sq(n: int) -> int:
    return n * n

def main() -> None:
    let xs: list[int] = [1, 2, 3, 4, 5]
    print([sq(x) for x in xs])

if __name__ == \"__main__\":
    main()
";
    let tmp = tempfile::tempdir().unwrap();
    scaffold_parallel_full(tmp.path(), "interpreters", program);
    build(tmp.path());
    let out = run_main(tmp.path()).expect("interpreters build should run (falls back to threads)");
    assert_eq!(out.trim(), "[1, 4, 9, 16, 25]", "unexpected output: {out}");
}

#[test]
fn interpreters_backend_probe_rejects_lambda_before_pool_creation() {
    // Regression for the P1 fallback hole: pickling a lambda raises
    // `pickle.PicklingError` (module level) or `AttributeError` (closure) —
    // NOT the `TypeError` the pool-block catch handles — so before the
    // pre-flight probe, a 3.14+ runtime (InterpreterPoolExecutor importable)
    // crashed instead of falling back to threads. Simulate that runtime on
    // any Python by injecting a fake executor whose *construction* aborts:
    // the probe must reject the lambda first, never touch the pool, and the
    // thread fallback must produce the correct result.
    let Some(py) = python() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    scaffold_parallel_full(
        tmp.path(),
        "interpreters",
        "@pure\ndef sq(n: int) -> int:\n    return n * n\n\n\
         xs: list[int] = [1, 2, 3]\nys: list[int] = [sq(x) for x in xs]\n",
    );
    build(tmp.path());
    let driver = r#"
import sys
import concurrent.futures as cf

class _MustNotConstruct:
    def __init__(self, *args, **kwargs):
        raise SystemExit("BUG: pool constructed for an unpicklable lambda")

# Simulate Python 3.14+: make `from concurrent.futures import
# InterpreterPoolExecutor` succeed, so only the pickle probe stands between
# a lambda and the (fatal) pool construction.
cf.InterpreterPoolExecutor = _MustNotConstruct

sys.path.insert(0, sys.argv[1])
from typhon_runtime.parallel import map_pure, _BACKEND

assert _BACKEND == "interpreters", _BACKEND
print(map_pure(lambda x: x * x, [1, 2, 3, 4, 5]))

# The probe must only guard serialisation: an exception raised BY the mapped
# function (here on the thread path a lambda falls back to) must propagate,
# never be swallowed by the fallback machinery.
try:
    map_pure(lambda x: 1 // (x - x), [7])
except ZeroDivisionError:
    pass
else:
    raise SystemExit("BUG: worker exception was swallowed")
"#;
    let driver_path = tmp.path().join("probe_driver.py");
    std::fs::write(&driver_path, driver).unwrap();
    let out = Command::new(py)
        .arg(&driver_path)
        .arg(tmp.path().join("build"))
        .output()
        .expect("python should spawn");
    assert!(
        out.status.success(),
        "probe driver crashed (fallback hole regressed):\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "[1, 4, 9, 16, 25]",
        "thread fallback produced wrong output"
    );
}

// ── REPL dedent terminator ──────────────────────────────────────────────────

#[test]
fn repl_dedent_terminator_closes_block_and_runs_next_line() {
    // Typed by hand:
    //   def greet():
    //       print("hi")
    //   print("after")     ← dedent-to-0 terminator
    // The REPL should treat the first two lines as one `def` block, then
    // execute `print("after")` as a fresh top-level statement. Stdout
    // should include "after"; "hi" only fires when `greet()` is called.
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
        .write_all(b"def greet():\n    print(\"hi\")\nprint(\"after\")\n:quit\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "repl should exit 0; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("after"),
        "dedent-terminated block should let the next statement run; got:\n{stdout}"
    );
}

#[test]
fn repl_backslash_continuation_preserves_payload() {
    // Backslash continuation joins two physical lines into one logical
    // statement.  The dedent rule must NOT apply here — `2` on column 0
    // is legitimate continuation content, not a sibling top-level
    // statement.  The eventual `print(x)` should see x = 1 + 2 = 3.
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
        .write_all(b"x = 1 + \\\n2\n\nprint(x)\n:quit\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "repl should exit 0; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains('3'),
        "backslash continuation should yield x=3; got:\n{stdout}"
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
    let formatted = script.replacen(
        "%r",
        &format!("{:?}", build_dir.to_string_lossy().into_owned()),
        1,
    );
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
        build_dir = build_dir.to_string_lossy().into_owned()
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
        build_dir = build_dir.to_string_lossy().into_owned()
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

#[test]
fn ok_and_err_expose_map_and_map_err_methods() {
    // R2-6: heterogeneous-error with-chains were forced into a 4-deep
    // match tower because the `?` operator demands a single error type
    // across every callee. Surfacing `.map_err(f)` (and the symmetric
    // `.map(f)`, `.and_then(f)`, `.or_else(f)`) as methods on Ok/Err
    // lets users normalise the error at each step in chain form:
    //
    //   tokenize(src).map_err(_lex_to_pipeline)?
    //   parse(toks).map_err(_parse_to_pipeline)?
    //
    // Verify the generated runtime exposes these methods and that
    // they behave per the algebra.
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "def f() -> Ok[int]:\n    return Ok(1)\n");
    build(tmp.path());
    let init =
        std::fs::read_to_string(tmp.path().join("build/typhon_runtime/__init__.py")).unwrap();
    for method in &["def map(", "def map_err(", "def and_then(", "def or_else("] {
        assert!(
            init.contains(method),
            "Ok / Err must expose `{method}…` method; got:\n{init}"
        );
    }

    // Algebraic round-trip: Ok.map_err is identity, Err.map_err transforms.
    let Some(py) = python() else { return };
    let build_dir = tmp.path().join("build");
    let script = format!(
        "\
import sys
sys.path.insert(0, {build_dir:?})
from typhon_runtime import Ok, Err
assert Ok(1).map_err(str) == Ok(1), 'Ok.map_err should be identity'
assert Err('oops').map_err(lambda e: e.upper()) == Err('OOPS'), 'Err.map_err must transform'
assert Ok(2).map(lambda x: x + 1) == Ok(3), 'Ok.map must transform'
assert Err('x').map(lambda v: v) == Err('x'), 'Err.map should be identity'
assert Ok(5).and_then(lambda x: Ok(x * 2)) == Ok(10), 'Ok.and_then must chain'
assert Err('e').and_then(lambda v: Ok(v)) == Err('e'), 'Err.and_then short-circuits'
assert Err('e').or_else(lambda e: Ok(len(e))) == Ok(1), 'Err.or_else recovers'
assert Ok(7).or_else(lambda e: Err('unused')) == Ok(7), 'Ok.or_else short-circuits'
print('OK')
",
        build_dir = build_dir.to_string_lossy().into_owned()
    );
    let out = Command::new(py).arg("-c").arg(&script).output().unwrap();
    assert!(
        out.status.success(),
        "Result method algebra failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
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
    // `tyc init NAME --dir DIR` scaffolds into `DIR/NAME/`, matching
    // `cargo new NAME` / `bun init NAME` / `uv init NAME` conventions.
    let toml = std::fs::read_to_string(sub.join("specific").join("typhon.toml")).unwrap();
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
    let status = tyc()
        .arg("check")
        .arg(tmp.path().join("demo"))
        .status()
        .unwrap();
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
    let project = tmp.path().join("demo");
    let status = tyc().arg("build").arg(&project).status().unwrap();
    assert!(status.success(), "scaffolded project should build");
    let out = Command::new(py)
        .arg(project.join("build").join("main.py"))
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

// ── corpus round-trip — Phase 3+ features ───────────────────────────────────

/// Exercises generics (PEP 695 syntax), interface (→ Protocol), @pure,
/// nullable narrowing, and Ok[T] together through `tyc check`.
///
/// Note: nullable narrowing uses `if r is not None:` (positive narrowing)
/// which is what the checker supports; negative early-return narrowing
/// (`if r is None: return`) is a follow-up.
///
/// Per FINDINGS #73 `from typing import TypeVar` is now rejected — this
/// test was updated to use `def identity[T](x: T) -> T:` instead.
#[test]
fn corpus_phase3_features_check_clean() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(
        tmp.path(),
        "interface Describable:\n\
         \x20   def describe(self) -> str:\n\
         \x20       ...\n\
         \n\
         class Success:\n\
         \x20   value: int\n\
         \x20   def describe(self) -> str:\n\
         \x20       return \"ok: \" + str(self.value)\n\
         \n\
         class Failure:\n\
         \x20   reason: str\n\
         \x20   def describe(self) -> str:\n\
         \x20       return \"fail: \" + self.reason\n\
         \n\
         @pure\n\
         def identity[T](x: T) -> T:\n\
         \x20   return x\n\
         \n\
         def format_opt(r: Success?) -> str:\n\
         \x20   if r is not None:\n\
         \x20       return \"ok: \" + str(r.value)\n\
         \x20   return \"none\"\n\
         \n\
         def safe_div(a: int, b: int) -> Ok[int]:\n\
         \x20   if b == 0:\n\
         \x20       return Ok(0)\n\
         \x20   return Ok(a)\n",
    );
    let status = tyc().arg("check").arg(tmp.path()).status().unwrap();
    assert!(
        status.success(),
        "Phase 3+ corpus program should type-check cleanly"
    );
}

/// Builds and runs a program that exercises interface, @pure, nullable
/// narrowing via field access, and class methods together through the full
/// emit pipeline.
///
/// Note: nullable narrowing uses `if p is not None:` (positive narrowing).
/// Method calls on the narrowed receiver work fine for non-nullable types.
#[test]
fn corpus_interface_pure_nullable_builds_and_runs() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(
        tmp.path(),
        "interface Describable:\n\
         \x20   def describe(self) -> str:\n\
         \x20       ...\n\
         \n\
         class Point:\n\
         \x20   x: int\n\
         \x20   y: int\n\
         \x20   def describe(self) -> str:\n\
         \x20       return \"(\" + str(self.x) + \", \" + str(self.y) + \")\"\n\
         \n\
         @pure\n\
         def negate(n: int) -> int:\n\
         \x20   return 0 - n\n\
         \n\
         def label(p: Point?) -> str:\n\
         \x20   if p is not None:\n\
         \x20       return str(p.x) + \", \" + str(p.y)\n\
         \x20   return \"nothing\"\n\
         \n\
         let origin: Point = Point(0, 0)\n\
         print(origin.describe())\n\
         print(label(Point(3, 4)))\n\
         print(label(None))\n\
         print(negate(7))\n",
    );
    let Some(out) = build_and_run_main(tmp.path()) else {
        return;
    };
    assert!(
        out.contains("(0, 0)"),
        "Point.describe() on a non-nullable should yield `(0, 0)`; got:\n{out}"
    );
    assert!(
        out.contains("3, 4"),
        "label(Point(3, 4)) should yield `3, 4`; got:\n{out}"
    );
    assert!(
        out.contains("nothing"),
        "label(None) should yield `nothing`; got:\n{out}"
    );
    assert!(
        out.contains("-7"),
        "negate(7) should yield `-7`; got:\n{out}"
    );
}

/// Ensures the Ok[T] / Err result type round-trips through emit and
/// executes correctly when combined with nullable arguments.
#[test]
fn corpus_result_type_with_nullable_builds_and_runs() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(
        tmp.path(),
        "def safe_div(a: int, b: int?) -> Ok[int]:\n\
         \x20   if b is None:\n\
         \x20       return Ok(0)\n\
         \x20   if b == 0:\n\
         \x20       return Ok(0)\n\
         \x20   return Ok(a)\n\
         \n\
         r = safe_div(10, 2)\n\
         print(r.value)\n\
         r2 = safe_div(10, None)\n\
         print(r2.value)\n",
    );
    let Some(out) = build_and_run_main(tmp.path()) else {
        return;
    };
    assert!(
        out.contains("10"),
        "safe_div(10, 2) should yield 10; got:\n{out}"
    );
    assert!(
        out.lines().filter(|l| l.trim() == "0").count() >= 1,
        "safe_div(10, None) should yield 0; got:\n{out}"
    );
}

// ── class! __init__ synthesis ──────────────────────────────────────────────

#[test]
fn raw_class_strips_field_defaults_when_init_is_synthesised() {
    // `class!` folds field defaults into the synthesised `__init__`
    // signature; leaving them at class scope as well would evaluate
    // each default twice (once as a shared class attribute at
    // class-definition time, once per-instance inside `__init__`),
    // which allocates extra objects and confuses libraries that
    // introspect class attributes (e.g. PyTorch parameter
    // registration on subclasses).
    let tmp = tempfile::tempdir().unwrap();
    scaffold(
        tmp.path(),
        "from torch.nn import Module, Linear, F, Tensor\n\
         \n\
         class! Model(Module):\n\
         \x20   linear: Linear = Linear(10, 5)\n\
         \x20   dropout: float = 0.5\n\
         \n\
         impl Model:\n\
         \x20   def forward(self, x: Tensor) -> Tensor:\n\
         \x20       mut x = self.linear(x)\n\
         \x20       x = F.dropout(x, p=self.dropout)\n\
         \x20       return x\n",
    );
    build(tmp.path());
    let out = std::fs::read_to_string(tmp.path().join("build/main.py")).unwrap();
    // Annotations survive so type checkers still see the field shape.
    assert!(
        out.contains("linear: Linear\n") || out.contains("linear: Linear\r\n"),
        "bare `linear: Linear` annotation should remain after stripping default; got:\n{out}",
    );
    assert!(
        out.contains("dropout: float\n") || out.contains("dropout: float\r\n"),
        "bare `dropout: float` annotation should remain after stripping default; got:\n{out}",
    );
    // The default expression must not survive at class scope — the
    // `__init__` signature is the single source of truth.
    let class_body_end = out.find("def __init__").unwrap_or(out.len());
    let class_body = &out[..class_body_end];
    assert!(
        !class_body.contains("Linear(10, 5)"),
        "class-level `linear: Linear = Linear(10, 5)` default should be stripped; got class body:\n{class_body}",
    );
    assert!(
        !class_body.contains("dropout: float = 0.5"),
        "class-level `dropout: float = 0.5` default should be stripped; got class body:\n{class_body}",
    );
    // But the `__init__` signature still carries them as parameter defaults.
    assert!(
        out.contains("def __init__(self, linear: Linear = Linear(10, 5), dropout: float = 0.5)"),
        "synthesised __init__ should carry the defaults; got:\n{out}",
    );
    // And the per-instance assignments remain in source order.
    assert!(
        out.contains("self.linear = linear"),
        "synthesised __init__ should assign self.linear; got:\n{out}",
    );
    assert!(
        out.contains("self.dropout = dropout"),
        "synthesised __init__ should assign self.dropout; got:\n{out}",
    );
}

#[test]
fn plain_class_keeps_field_defaults() {
    // The default-stripping rewrite is scoped to `class!` synthesis.
    // A plain `class` lowers to `@dataclass(slots=True)`, where the
    // class-level default *is* the source of the field default — it
    // must not be stripped.
    let tmp = tempfile::tempdir().unwrap();
    scaffold(
        tmp.path(),
        "class Point:\n\
         \x20   x: float = 0.0\n\
         \x20   y: float = 0.0\n",
    );
    build(tmp.path());
    let out = std::fs::read_to_string(tmp.path().join("build/main.py")).unwrap();
    assert!(
        out.contains("x: float = 0.0") && out.contains("y: float = 0.0"),
        "plain `class` must keep class-level defaults — they feed @dataclass; got:\n{out}",
    );
}

#[test]
fn raw_class_without_base_keeps_field_defaults() {
    // The synthesis only fires for `class!` with at least one
    // positional base (something to chain `super().__init__()`
    // through). A bare `class! Foo:` falls through without synthesis,
    // so defaults must survive.
    let tmp = tempfile::tempdir().unwrap();
    scaffold(
        tmp.path(),
        "class! Empty:\n\
         \x20   value: int = 42\n",
    );
    build(tmp.path());
    let out = std::fs::read_to_string(tmp.path().join("build/main.py")).unwrap();
    assert!(
        out.contains("value: int = 42"),
        "`class!` without a base does not synthesise __init__, so the default must remain; got:\n{out}",
    );
    assert!(
        !out.contains("def __init__"),
        "`class!` without a base must not synthesise __init__; got:\n{out}",
    );
}

#[test]
fn raw_class_with_base_no_fields_synthesises_passthrough_init() {
    // The conventional `class! AppError(Exception): pass` shape must
    // synthesise a `super()`-passthrough constructor so that
    // `raise AppError("boom")` reaches `Exception.__init__("boom")`.
    // Without it, the generated `__init__(self)` would reject the
    // positional argument.
    let tmp = tempfile::tempdir().unwrap();
    scaffold(
        tmp.path(),
        "class! AppError(Exception):\n\
         \x20   pass\n\
         \n\
         def main() -> None:\n\
         \x20   try:\n\
         \x20       raise AppError(\"boom\")\n\
         \x20   except AppError as e:\n\
         \x20       assert str(e) == \"boom\", f\"got: {e!r}\"\n\
         \n\
         if __name__ == \"__main__\":\n\
         \x20   main()\n",
    );
    // `build_and_run_main` skips cleanly on hosts without a 3.12+
    // Python on PATH (which is how every other test in this file
    // guards its execution step) and uses the resolved interpreter
    // rather than hard-coded `python`, so CI on systems where only
    // `python3` exists still passes.
    let Some(_) = build_and_run_main(tmp.path()) else {
        return;
    };
    let out = std::fs::read_to_string(tmp.path().join("build/main.py")).unwrap();
    assert!(
        out.contains("def __init__(self, *args, **kwargs) -> None:"),
        "fieldless `class!` with a base should synthesise a *args/**kwargs init; got:\n{out}",
    );
    assert!(
        out.contains("super(AppError, self).__init__(*args, **kwargs)"),
        "synthesised passthrough init must forward to super; got:\n{out}",
    );
}

// ── pyproject.toml bootstrap on `tyc build` ────────────────────────────────

#[test]
fn build_bootstraps_pyproject_when_missing() {
    // `tyc build` should write a fresh pyproject.toml derived from
    // typhon.toml when the project doesn't have one yet. This is the
    // greenfield path — no merging, just a clean greenfield render.
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "def main() -> None:\n    print(1)\n");
    build(tmp.path());
    let pyproject = tmp.path().join("pyproject.toml");
    assert!(
        pyproject.exists(),
        "tyc build should create pyproject.toml when missing"
    );
    let text = std::fs::read_to_string(&pyproject).unwrap();
    assert!(text.contains("name = \"feat\""), "{text}");
    assert!(text.contains("requires-python"), "{text}");
}

#[test]
fn build_preserves_user_tool_tables_in_pyproject() {
    // The key promise of "merge-aware" bootstrap: user-owned tables
    // like [tool.ruff] survive. If this ever regresses to a full
    // overwrite, every downstream user with a hand-written
    // pyproject.toml loses their config silently — which is much
    // worse than failing loudly.
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "def main() -> None:\n    print(1)\n");
    std::fs::write(
        tmp.path().join("pyproject.toml"),
        "# my header\n\
         [project]\n\
         name = \"will-be-overwritten\"\n\
         version = \"9.9.9\"\n\
         authors = [{ name = \"H\" }]\n\
         readme = \"README.md\"\n\
         \n\
         [tool.ruff]\n\
         line-length = 100\n\
         \n\
         [tool.pytest.ini_options]\n\
         testpaths = [\"tests\"]\n",
    )
    .unwrap();
    build(tmp.path());
    let text = std::fs::read_to_string(tmp.path().join("pyproject.toml")).unwrap();
    // Our owned keys overwrite the user's stale values.
    assert!(
        text.contains("name = \"feat\""),
        "owned `name` should be rewritten; got:\n{text}",
    );
    assert!(
        !text.contains("will-be-overwritten") && !text.contains("9.9.9"),
        "stale owned values must be gone; got:\n{text}",
    );
    // Header and user-managed [project] keys survive.
    assert!(
        text.starts_with("# my header\n"),
        "header comment must be preserved; got:\n{text}",
    );
    assert!(
        text.contains("authors") && text.contains("\"H\""),
        "user `authors` must survive; got:\n{text}",
    );
    assert!(
        text.contains("readme = \"README.md\""),
        "user `readme` must survive; got:\n{text}",
    );
    // [tool.*] tables are entirely user-owned.
    assert!(
        text.contains("[tool.ruff]") && text.contains("line-length = 100"),
        "[tool.ruff] must survive; got:\n{text}",
    );
    assert!(
        text.contains("[tool.pytest.ini_options]") && text.contains("testpaths"),
        "[tool.pytest.ini_options] must survive; got:\n{text}",
    );
}

#[test]
fn build_emits_typhon_runtime_when_only_lazy_import_used() {
    // A module whose only runtime contact is `lazy import` must still
    // get `build/typhon_runtime/` emitted — the lowering injects
    // `from typhon_runtime.lazy import lazy_import as ...`, so the
    // generated Python imports the runtime package at startup. An
    // earlier regression matched only the bare `typhon_runtime`
    // module name and missed dotted submodule imports, producing a
    // build that fails at startup with `ModuleNotFoundError`.
    let tmp = tempfile::tempdir().unwrap();
    scaffold(
        tmp.path(),
        "lazy import np = numpy\nlet arr: object = np.array([1])\n",
    );
    build(tmp.path());
    assert!(
        tmp.path().join("build/main.py").exists(),
        "main.py must be emitted",
    );
    assert!(
        tmp.path().join("build/typhon_runtime").exists(),
        "typhon_runtime/ package must be emitted when only `lazy import` is used \
         (the injected `from typhon_runtime.lazy import …` would fail at startup otherwise)",
    );
    assert!(
        tmp.path().join("build/typhon_runtime/lazy.py").exists(),
        "typhon_runtime/lazy.py must ship with the package",
    );
}

// ── PEP 810 native lazy imports (3.15+ targets) ───────────────────────────────

#[test]
fn build_lazy_import_lowers_to_native_pep810_on_3_15() {
    // On a 3.15 target, `lazy import ALIAS = MODULE` lowers to the native
    // deferred-import syntax `lazy import MODULE as ALIAS` — not the
    // runtime-helper call (nor the historical `__TyphonLazy_` proxy class).
    let tmp = tempfile::tempdir().unwrap();
    scaffold_target(
        tmp.path(),
        "3.15",
        "lazy import np = numpy\nlet arr: object = np.array([1])\n",
    );
    build(tmp.path());
    let py = main_py(tmp.path());
    assert!(
        py.contains("lazy import numpy as np"),
        "3.15 target must emit native `lazy import MODULE as ALIAS`; got:\n{py}"
    );
    assert!(
        !py.contains("__TyphonLazy_"),
        "native lowering must not emit the historical proxy class; got:\n{py}"
    );
    assert!(
        !py.contains("__typhon_lazy_import"),
        "native lowering must not emit the runtime-helper call; got:\n{py}"
    );
    assert!(
        !py.contains("from typhon_runtime.lazy import"),
        "native lowering must not inject the runtime-helper import; got:\n{py}"
    );
}

#[test]
fn build_lazy_import_stays_runtime_helper_on_3_13() {
    // Regression pin: a 3.13 target is byte-for-byte unchanged by the PEP 810
    // work — it keeps the runtime-helper lowering and never emits the native
    // `lazy import MODULE as ALIAS` keyword form.
    let tmp = tempfile::tempdir().unwrap();
    scaffold_target(
        tmp.path(),
        "3.13",
        "lazy import np = numpy\nlet arr: object = np.array([1])\n",
    );
    build(tmp.path());
    let py = main_py(tmp.path());
    assert!(
        py.contains("from typhon_runtime.lazy import lazy_import as __typhon_lazy_import"),
        "3.13 target must keep the runtime-helper import; got:\n{py}"
    );
    assert!(
        py.contains("np = __typhon_lazy_import(\"numpy\")"),
        "3.13 target must keep the runtime-helper call; got:\n{py}"
    );
    // The native keyword form must NOT appear. (The injected header
    // `from typhon_runtime.lazy import …` contains the substring "lazy
    // import", so match the whole native statement, anchored at column 0.)
    assert!(
        !py.lines().any(|l| l.starts_with("lazy import ")),
        "3.13 target must not emit the native `lazy import` keyword form; got:\n{py}"
    );
}

#[test]
fn build_lazy_import_stays_runtime_helper_on_3_14() {
    // Native lowering is gated on `>= (3, 15)`, so 3.14 stays on the
    // runtime-helper path exactly like 3.13.
    let tmp = tempfile::tempdir().unwrap();
    scaffold_target(
        tmp.path(),
        "3.14",
        "lazy import np = numpy\nlet arr: object = np.array([1])\n",
    );
    build(tmp.path());
    let py = main_py(tmp.path());
    assert!(
        py.contains("np = __typhon_lazy_import(\"numpy\")"),
        "3.14 target must keep the runtime-helper call; got:\n{py}"
    );
    assert!(
        !py.lines().any(|l| l.starts_with("lazy import ")),
        "3.14 target must not emit the native `lazy import` keyword form; got:\n{py}"
    );
}

#[test]
fn build_lazy_import_alias_equals_module_native_on_3_15() {
    // The alias==module case: `lazy import numpy = numpy` lowers (via the
    // main preprocessor) to `import numpy as numpy`, which the emitter prints
    // verbatim (it never elides the redundant `as`). The native rewrite then
    // prefixes it to `lazy import numpy as numpy`.
    let tmp = tempfile::tempdir().unwrap();
    scaffold_target(
        tmp.path(),
        "3.15",
        "lazy import numpy = numpy\nlet arr: object = numpy.array([1])\n",
    );
    build(tmp.path());
    let py = main_py(tmp.path());
    assert!(
        py.contains("lazy import numpy as numpy"),
        "alias==module must emit `lazy import numpy as numpy`; got:\n{py}"
    );
    assert!(
        !py.contains("__typhon_lazy_import"),
        "alias==module native form must not emit the runtime helper; got:\n{py}"
    );
}

#[test]
fn build_lazy_import_dotted_module_native_on_3_15() {
    // Dotted submodule targets round-trip through the native form.
    let tmp = tempfile::tempdir().unwrap();
    scaffold_target(
        tmp.path(),
        "3.15",
        "lazy import nn = torch.nn\nlet layer: object = nn.Linear(1, 1)\n",
    );
    build(tmp.path());
    let py = main_py(tmp.path());
    assert!(
        py.contains("lazy import torch.nn as nn"),
        "dotted module must emit `lazy import torch.nn as nn`; got:\n{py}"
    );
}

#[test]
fn build_native_lazy_import_skips_typhon_runtime_on_3_15() {
    // Payoff of native lowering: when `lazy import` is the ONLY runtime-needing
    // feature, a 3.15 build no longer emits `typhon_runtime/` at all (there is
    // no `from typhon_runtime.lazy import …` to satisfy). Contrast with
    // `build_emits_typhon_runtime_when_only_lazy_import_used`, which pins the
    // opposite for the 3.13 runtime-helper path.
    let tmp = tempfile::tempdir().unwrap();
    scaffold_target(
        tmp.path(),
        "3.15",
        "lazy import np = numpy\nlet arr: object = np.array([1])\n",
    );
    build(tmp.path());
    assert!(
        tmp.path().join("build/main.py").exists(),
        "main.py must be emitted",
    );
    assert!(
        !tmp.path().join("build/typhon_runtime").exists(),
        "native lazy imports must not force `typhon_runtime/` emission on 3.15",
    );
}

#[test]
fn build_native_lazy_import_sourcemap_round_trips_on_3_15() {
    // The native rewrite only prepends `lazy ` to existing lines (no line-count
    // change), so the `.py.map` sidecar stays valid: its `lines` table has one
    // entry per emitted output line. (format=false keeps emit line count and
    // map entries in lock-step, unlike the ruff pass which may add blank lines.)
    let tmp = tempfile::tempdir().unwrap();
    scaffold_target(
        tmp.path(),
        "3.15",
        "lazy import np = numpy\nlet arr: object = np.array([1])\n",
    );
    build(tmp.path());
    let map_path = tmp
        .path()
        .join("build")
        .join(".sourcemaps")
        .join("main.py.map");
    assert!(
        map_path.exists(),
        "3.15 build must write the .py.map sidecar"
    );
    let body = std::fs::read_to_string(&map_path).unwrap();
    assert!(
        body.contains("\"version\":2"),
        "map must be v2; got: {body}"
    );
    assert!(
        body.contains("\"lines\":["),
        "map must carry a lines table; got: {body}"
    );

    // Parse the `lines` array and assert one entry per emitted output line.
    let arr = body
        .split("\"lines\":[")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .expect("lines array present");
    let map_entries = arr.split(',').filter(|s| !s.trim().is_empty()).count();
    let py_lines = main_py(tmp.path()).lines().count();
    assert_eq!(
        map_entries, py_lines,
        "source-map `lines` entries ({map_entries}) must match emitted line count ({py_lines})",
    );
}

#[test]
fn build_native_lazy_import_3_13_output_runs() {
    // The 3.13 runtime-helper path still produces runnable Python. Use a
    // stdlib module (`json`) so the lazy import resolves at first use without
    // any third-party dependency, and execute the emitted script.
    let tmp = tempfile::tempdir().unwrap();
    scaffold_target(
        tmp.path(),
        "3.13",
        "lazy import js = json\n\ndef main() -> None:\n    let payload: str = js.dumps({\"ok\": True})\n    print(payload)\n\nif __name__ == \"__main__\":\n    main()\n",
    );
    let Some(out) = build_and_run_main(tmp.path()) else {
        return; // no Python interpreter — skip cleanly
    };
    assert_eq!(out.trim(), "{\"ok\": true}");
}

#[test]
fn build_emits_codegen_artefact_regardless_of_bootstrap_outcome() {
    // The bootstrap step (pyproject.toml merge + `uv sync`) is
    // best-effort: a missing `uv`, a sync failure, or a transient
    // network error must not prevent the `.py` artefacts from
    // landing. The promise of `tyc build` is the codegen output.
    //
    // This test doesn't force `uv` off PATH (doing so reliably
    // across CI runners is fiddly — `PATH=""` breaks coreutils,
    // and overriding HOME/XDG_BIN can leak); it asserts the
    // baseline guarantee on whatever state the runner has. The
    // warning path itself is covered by the `run_uv_sync_warning`
    // implementation: missing uv → warning, sync failure →
    // warning, never a non-zero exit.
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "def main() -> None:\n    print(1)\n");
    build(tmp.path());
    assert!(
        tmp.path().join("build/main.py").exists(),
        "build artefact must be emitted even when bootstrap warns",
    );
}

// ── comptime type values ────────────────────────────────────────────────────

#[test]
fn comptime_type_value_evaluates_and_emits() {
    // B34: `comptime let T: type = int` lowers to the PEP 695
    // `type T = int` alias statement so the type-checker treats `T`
    // and `int` interchangeably in subsequent annotations.
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "comptime let T: type = int\n");
    build(tmp.path());
    let py = main_py(tmp.path());
    assert!(
        py.contains("type T = int"),
        "comptime type value should emit as PEP 695 type alias; got:\n{py}"
    );
}

#[test]
fn comptime_type_value_multiple_types() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(
        tmp.path(),
        "comptime let T1: type = str\ncomptime let T2: type = int\n",
    );
    build(tmp.path());
    let py = main_py(tmp.path());
    assert!(
        py.contains("type T1 = str"),
        "comptime type value for str should emit; got:\n{py}"
    );
    assert!(
        py.contains("type T2 = int"),
        "comptime type value for int should emit; got:\n{py}"
    );
}

#[test]
fn comptime_type_value_rejects_any_without_import() {
    // `Any` is not a runtime builtin; emitting it as a bare name would
    // cause NameError unless the module imported it from `typing`.
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "comptime let T: type = Any\n");
    let out = tyc()
        .args(["build", tmp.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "build must fail for comptime Any without import"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown name 'Any'"),
        "error should mention 'Any'; got:\n{stderr}"
    );
}

#[test]
fn comptime_type_value_allows_runtime_builtins() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(
        tmp.path(),
        "comptime let T1: type = int\n\
         comptime let T2: type = str\n\
         comptime let T3: type = bool\n\
         comptime let T4: type = float\n\
         comptime let T5: type = bytes\n\
         comptime let T6: type = type\n\
         comptime let T7: type = object\n",
    );
    build(tmp.path());
    let py = main_py(tmp.path());
    // After B34 each `comptime let TN: type = X` lowers to
    // `type TN = X` (PEP 695). Walk every builtin and check the
    // emitted alias is present.
    for (i, ty) in ["int", "str", "bool", "float", "bytes", "type", "object"]
        .iter()
        .enumerate()
    {
        let alias = format!("type T{} = {}", i + 1, ty);
        assert!(
            py.contains(&alias),
            "comptime type value for {ty} should emit as `{alias}`; got:\n{py}"
        );
    }
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
