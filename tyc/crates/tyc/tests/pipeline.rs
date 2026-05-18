//! End-to-end integration tests for the `tyc` CLI binary.
//!
//! Each test invokes the real `tyc` binary via `std::process::Command` using the
//! `CARGO_BIN_EXE_tyc` path that Cargo injects automatically for integration
//! test targets.  This validates that all CLI commands and their argument wiring
//! work correctly when called from the outside, not just as library functions.

use std::path::Path;
use std::process::Command;

// ── helpers ───────────────────────────────────────────────────────────────────

fn tyc() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tyc"))
}

/// Write a minimal `typhon.toml` + `src/main.ty` under `dir`.
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

// ── tyc init ─────────────────────────────────────────────────────────────────

#[test]
fn init_creates_project_structure() {
    let tmp = tempfile::tempdir().unwrap();
    let status = tyc()
        .args(["init", "myapp", "--dir"])
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(status.success(), "tyc init should succeed");
    assert!(
        tmp.path().join("typhon.toml").exists(),
        "typhon.toml missing"
    );
    assert!(
        tmp.path().join("src").join("main.ty").exists(),
        "src/main.ty missing"
    );
    assert!(tmp.path().join("tests").is_dir(), "tests/ dir missing");
}

#[test]
fn init_embeds_project_name_in_toml() {
    let tmp = tempfile::tempdir().unwrap();
    tyc()
        .args(["init", "coolproject", "--dir"])
        .arg(tmp.path())
        .status()
        .unwrap();
    let toml = std::fs::read_to_string(tmp.path().join("typhon.toml")).unwrap();
    assert!(
        toml.contains("coolproject"),
        "project name not present in typhon.toml"
    );
}

#[test]
fn init_rejects_existing_toml() {
    let tmp = tempfile::tempdir().unwrap();
    // First init must succeed.
    tyc()
        .args(["init", "proj", "--dir"])
        .arg(tmp.path())
        .status()
        .unwrap();
    // Second init on the same dir must fail.
    let status = tyc()
        .args(["init", "proj", "--dir"])
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "re-init of an existing project should fail"
    );
}

// ── tyc check ────────────────────────────────────────────────────────────────

#[test]
fn check_passes_on_valid_source() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("ok.ty"), "let x: int = 42\n").unwrap();
    let status = tyc().arg("check").arg(tmp.path()).status().unwrap();
    assert!(status.success(), "tyc check should pass on valid source");
}

#[test]
fn check_fails_on_type_error() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("bad.ty"), "let x: int = \"hello\"\n").unwrap();
    let status = tyc().arg("check").arg(tmp.path()).status().unwrap();
    assert!(
        !status.success(),
        "tyc check should fail on a type mismatch"
    );
}

#[test]
fn check_passes_nullable_annotation() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("nullable.ty"), "let x: str? = None\n").unwrap();
    let status = tyc().arg("check").arg(tmp.path()).status().unwrap();
    assert!(
        status.success(),
        "tyc check should accept T? (nullable) annotations"
    );
}

// ── tyc fmt ──────────────────────────────────────────────────────────────────

#[test]
fn fmt_check_passes_on_already_formatted_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("x.ty"), "let x: int = 1\n").unwrap();
    let status = tyc()
        .args(["fmt", "--check"])
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(
        status.success(),
        "tyc fmt --check should succeed when no changes needed"
    );
}

#[test]
fn fmt_check_fails_on_unformatted_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("y.ty"), "def f():\n\tpass\n").unwrap();
    let status = tyc()
        .args(["fmt", "--check"])
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "tyc fmt --check should fail when a file would be reformatted"
    );
}

#[test]
fn fmt_rewrites_tab_indentation_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("z.ty");
    std::fs::write(&path, "def f():\n\tpass\n").unwrap();
    let status = tyc().arg("fmt").arg(&path).status().unwrap();
    assert!(status.success(), "tyc fmt should succeed");
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(
        content.contains("    pass"),
        "tab indentation should be rewritten to spaces"
    );
}

// ── tyc build ────────────────────────────────────────────────────────────────

#[test]
fn build_produces_py_file_from_simple_source() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "let greeting: str = \"hello\"\n");
    let status = tyc().arg("build").arg(tmp.path()).status().unwrap();
    assert!(status.success(), "tyc build should succeed");
    assert!(
        tmp.path().join("build").join("main.py").exists(),
        "build/main.py should be emitted"
    );
}

#[test]
fn build_fails_on_type_error() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "let x: int = \"wrong type\"\n");
    let status = tyc().arg("build").arg(tmp.path()).status().unwrap();
    assert!(
        !status.success(),
        "tyc build should fail when there is a type mismatch"
    );
}

#[test]
fn build_emits_dataclass_decorator_for_class() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "class Point:\n    x: int\n    y: int\n");
    let status = tyc().arg("build").arg(tmp.path()).status().unwrap();
    assert!(status.success(), "build should succeed for a plain class");
    let out = std::fs::read_to_string(tmp.path().join("build").join("main.py")).unwrap();
    assert!(
        out.contains("@dataclasses.dataclass"),
        "class should be emitted as a @dataclasses.dataclass"
    );
}

#[test]
fn build_emits_typhon_runtime_when_result_used() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "def f() -> Ok[int]:\n    return Ok(1)\n");
    tyc().arg("build").arg(tmp.path()).status().unwrap();
    let runtime_pkg = tmp.path().join("build").join("typhon_runtime");
    assert!(
        runtime_pkg.join("__init__.py").exists(),
        "typhon_runtime/__init__.py should be emitted when Ok/Err/Result are used"
    );
    assert!(
        runtime_pkg.join("tasks.py").exists(),
        "typhon_runtime/tasks.py should be emitted"
    );
    assert!(
        runtime_pkg.join("lazy.py").exists(),
        "typhon_runtime/lazy.py should be emitted"
    );
}

// ── tyc trace ────────────────────────────────────────────────────────────────

#[test]
fn trace_exits_zero_with_no_input() {
    let status = tyc().arg("trace").status().unwrap();
    assert!(status.success(), "tyc trace should exit 0 with no input");
}

#[test]
fn trace_passes_through_non_frame_lines() {
    // A traceback with no `.py` file references should be printed unchanged.
    let dir = tempfile::tempdir().unwrap();
    let tb_path = dir.path().join("tb.txt");
    std::fs::write(
        &tb_path,
        "Traceback (most recent call last):\nValueError: oops\n",
    )
    .unwrap();
    let out = tyc().arg("trace").arg(&tb_path).output().unwrap();
    assert!(out.status.success(), "tyc trace should succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Traceback"),
        "header line should be preserved; got: {stdout}"
    );
    assert!(
        stdout.contains("ValueError"),
        "exception line should be preserved; got: {stdout}"
    );
}

#[test]
fn trace_rewrites_frame_with_map_file() {
    // Build a real project so `tyc build` emits a `.py.map` sidecar, then
    // feed a synthetic traceback pointing at the emitted `.py` to
    // `tyc trace` and verify the path is rewritten to the `.ty` source.
    //
    // Use two annotated-assignment statements so Python lines 1 and 2 map
    // directly to ty lines 1 and 2 (no leading blank line that a function
    // definition would insert).
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "x: int = 1\ny: str = \"hello\"\n");
    let build_status = tyc().arg("build").arg(tmp.path()).status().unwrap();
    assert!(
        build_status.success(),
        "build should succeed before trace test"
    );

    let py_path = tmp.path().join("build").join("main.py");
    let map_path = tmp.path().join("build").join("main.py.map");
    assert!(py_path.exists(), "main.py should exist after build");
    assert!(map_path.exists(), "main.py.map should exist after build");

    // Write a synthetic traceback referencing line 2 of the built .py file.
    // The v2 source map maps Python line 2 → ty line 2, so the line number
    // is preserved in the rewritten output.
    let tb = format!(
        "Traceback (most recent call last):\n  File \"{}\", line 2, in <module>\n    y: str = \"hello\"\n",
        py_path.display()
    );
    let tb_path = tmp.path().join("tb.txt");
    std::fs::write(&tb_path, &tb).unwrap();

    let out = tyc().arg("trace").arg(&tb_path).output().unwrap();
    assert!(out.status.success(), "tyc trace should succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("main.ty"),
        "trace should rewrite .py path to .ty; got: {stdout}"
    );
    assert!(
        stdout.contains("line 2"),
        "line number should be preserved; got: {stdout}"
    );
    assert!(
        !stdout.contains("main.py\""),
        ".py path should be replaced; got: {stdout}"
    );
}

// ── tyc profile ───────────────────────────────────────────────────────────────

#[test]
fn profile_instruments_scaffolded_project() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "def greet() -> str:\n    return \"hello\"\n");
    let status = tyc().arg("profile").arg(tmp.path()).status().unwrap();
    assert!(
        status.success(),
        "tyc profile should succeed on a valid project"
    );
    assert!(
        tmp.path().join("build").join("typhon_profile.py").exists(),
        "typhon_profile.py should be dropped into the build dir"
    );
}

#[test]
fn profile_decorates_top_level_functions() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "def greet() -> str:\n    return \"hello\"\n");
    let status = tyc().arg("profile").arg(tmp.path()).status().unwrap();
    assert!(status.success(), "tyc profile should succeed");
    let py = std::fs::read_to_string(tmp.path().join("build").join("main.py")).unwrap();
    assert!(
        py.contains("@__typhon_profile_record"),
        "top-level functions should be decorated with @__typhon_profile_record; got:\n{py}"
    );
}

// ── full pipeline ─────────────────────────────────────────────────────────────

/// Full pipeline smoke test: init → write Phase 3 source → check → build → verify output.
///
/// Exercises: `interface` (→ `Protocol`), sealed union type alias, `@pure`
/// function, and class desugaring — the four key Phase 3 features.
#[test]
fn full_pipeline_phase3_features() {
    let tmp = tempfile::tempdir().unwrap();

    // Scaffold the project via `tyc init`.
    let status = tyc()
        .args(["init", "demo", "--dir"])
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(status.success(), "tyc init failed");

    // Replace the generated main.ty with a Phase 3 feature showcase:
    //   - `interface` declaration (→ Protocol class)
    //   - two concrete classes conforming to the interface
    //   - sealed union type alias (`type AnyGreeter = ...`)
    //   - `@pure` function
    let src = r#"
interface Greeter:
    def greet(self) -> str:
        ...

class EnglishGreeter:
    def greet(self) -> str:
        return "Hello"

class SpanishGreeter:
    def greet(self) -> str:
        return "Hola"

type AnyGreeter = EnglishGreeter | SpanishGreeter

@pure
def double(x: int) -> int:
    return x * 2

let g: Greeter = EnglishGreeter()
let result: int = double(21)
"#;
    std::fs::write(tmp.path().join("src").join("main.ty"), src).unwrap();

    // `tyc check` must pass.
    let status = tyc().arg("check").arg(tmp.path()).status().unwrap();
    assert!(status.success(), "tyc check failed on Phase 3 fixture");

    // `tyc build` must succeed and produce main.py.
    let status = tyc().arg("build").arg(tmp.path()).status().unwrap();
    assert!(status.success(), "tyc build failed on Phase 3 fixture");

    let py = std::fs::read_to_string(tmp.path().join("build").join("main.py")).unwrap();
    assert!(
        py.contains("Protocol"),
        "interface should be emitted as a Protocol class"
    );
    assert!(
        py.contains("@dataclasses.dataclass"),
        "concrete classes should be emitted as dataclasses"
    );
    assert!(
        py.contains("double"),
        "pure function should appear in emitted output"
    );
}

// ── tyc migrate ───────────────────────────────────────────────────────────────

#[test]
fn migrate_rewrites_optional_annotation() {
    let tmp = tempfile::tempdir().unwrap();
    let py_path = tmp.path().join("app.py");
    std::fs::write(
        &py_path,
        "from typing import Optional\n\nname: Optional[str] = None\n",
    )
    .unwrap();

    let status = tyc().arg("migrate").arg(&py_path).status().unwrap();
    assert!(status.success(), "tyc migrate should succeed on valid Python");

    let ty_path = tmp.path().join("app.ty");
    assert!(ty_path.exists(), "tyc migrate should produce a .ty file");

    let ty_src = std::fs::read_to_string(&ty_path).unwrap();
    assert!(
        ty_src.contains("str?"),
        "Optional[str] should be rewritten to str?; got:\n{ty_src}"
    );
}

#[test]
fn migrate_adds_let_to_module_level_assignment() {
    let tmp = tempfile::tempdir().unwrap();
    let py_path = tmp.path().join("constants.py");
    std::fs::write(&py_path, "PORT: int = 8080\nHOST: str = \"localhost\"\n").unwrap();

    let status = tyc().arg("migrate").arg(&py_path).status().unwrap();
    assert!(status.success(), "tyc migrate should succeed");

    let ty_src = std::fs::read_to_string(tmp.path().join("constants.ty")).unwrap();
    assert!(
        ty_src.contains("let PORT"),
        "module-level annotated assign should gain `let`; got:\n{ty_src}"
    );
    assert!(
        ty_src.contains("let HOST"),
        "module-level annotated assign should gain `let`; got:\n{ty_src}"
    );
}

#[test]
fn migrate_drops_dataclass_decorator() {
    let tmp = tempfile::tempdir().unwrap();
    let py_path = tmp.path().join("model.py");
    std::fs::write(
        &py_path,
        "from dataclasses import dataclass\n\n@dataclass\nclass Point:\n    x: int\n    y: int\n",
    )
    .unwrap();

    let status = tyc().arg("migrate").arg(&py_path).status().unwrap();
    assert!(status.success(), "tyc migrate should succeed");

    let ty_src = std::fs::read_to_string(tmp.path().join("model.ty")).unwrap();
    assert!(
        !ty_src.contains("@dataclass"),
        "dataclass decorator should be removed; got:\n{ty_src}"
    );
    assert!(
        ty_src.contains("class Point"),
        "class declaration should be preserved; got:\n{ty_src}"
    );
}

#[test]
fn migrate_check_mode_writes_to_stdout_not_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let py_path = tmp.path().join("thing.py");
    std::fs::write(&py_path, "x: int = 1\n").unwrap();

    let out = tyc()
        .args(["migrate", "--check"])
        .arg(&py_path)
        .output()
        .unwrap();
    assert!(out.status.success(), "tyc migrate --check should succeed");

    // The .ty file must NOT be written in --check mode.
    assert!(
        !tmp.path().join("thing.ty").exists(),
        "tyc migrate --check must not write a .ty file"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("let x"),
        "migrated source should appear on stdout; got:\n{stdout}"
    );
}

#[test]
fn migrate_missing_path_errors() {
    let status = tyc()
        .arg("migrate")
        .arg("/no/such/path/does/not/exist.py")
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "migrate of a missing path should fail with non-zero exit"
    );
}

// ── tyc check --stubs ─────────────────────────────────────────────────────────

#[test]
fn check_stubs_passes_when_stub_matches_implementation() {
    let tmp = tempfile::tempdir().unwrap();
    // Write a .ty implementation file.
    std::fs::write(
        tmp.path().join("math.ty"),
        "def add(a: int, b: int) -> int:\n    return a + b\n",
    )
    .unwrap();
    // Write a matching .dty stub.
    std::fs::write(
        tmp.path().join("math.dty"),
        "def add(a: int, b: int) -> int: ...\n",
    )
    .unwrap();

    let status = tyc()
        .arg("check")
        .arg("--stubs")
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(
        status.success(),
        "check --stubs should pass when stub matches implementation"
    );
}

#[test]
fn check_stubs_fails_when_stub_declares_missing_function() {
    let tmp = tempfile::tempdir().unwrap();
    // Implementation has `add` only.
    std::fs::write(
        tmp.path().join("math.ty"),
        "def add(a: int, b: int) -> int:\n    return a + b\n",
    )
    .unwrap();
    // Stub declares `add` and an extra `sub` that doesn't exist.
    std::fs::write(
        tmp.path().join("math.dty"),
        "def add(a: int, b: int) -> int: ...\ndef sub(a: int, b: int) -> int: ...\n",
    )
    .unwrap();

    let status = tyc()
        .arg("check")
        .arg("--stubs")
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "check --stubs should fail when stub declares a function absent from the implementation"
    );
}

#[test]
fn check_stubs_fails_when_implementation_has_extra_public_function() {
    let tmp = tempfile::tempdir().unwrap();
    // Implementation has both `add` and `mul`; stub only declares `add`.
    std::fs::write(
        tmp.path().join("math.ty"),
        "def add(a: int, b: int) -> int:\n    return a + b\n\ndef mul(a: int, b: int) -> int:\n    return a * b\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("math.dty"),
        "def add(a: int, b: int) -> int: ...\n",
    )
    .unwrap();

    let status = tyc()
        .arg("check")
        .arg("--stubs")
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "check --stubs should fail when implementation has a public function not in the stub"
    );
}

#[test]
fn check_stubs_standalone_dty_without_implementation_passes() {
    // A .dty with no sibling .ty/.py is valid — it may stub an external library.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("external.dty"),
        "def fetch(url: str) -> str: ...\n",
    )
    .unwrap();

    let status = tyc()
        .arg("check")
        .arg("--stubs")
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(
        status.success(),
        "standalone .dty with no implementation should pass --stubs"
    );
}
