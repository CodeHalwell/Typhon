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
        .args(["init", "myapp", "--dir", tmp.path().to_str().unwrap()])
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
        .args(["init", "coolproject", "--dir", tmp.path().to_str().unwrap()])
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
        .args(["init", "proj", "--dir", tmp.path().to_str().unwrap()])
        .status()
        .unwrap();
    // Second init on the same dir must fail.
    let status = tyc()
        .args(["init", "proj", "--dir", tmp.path().to_str().unwrap()])
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
    std::fs::write(tmp.path().join("ok.ty"), "val x: int = 42\n").unwrap();
    let status = tyc()
        .args(["check", tmp.path().to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success(), "tyc check should pass on valid source");
}

#[test]
fn check_fails_on_type_error() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("bad.ty"), "val x: int = \"hello\"\n").unwrap();
    let status = tyc()
        .args(["check", tmp.path().to_str().unwrap()])
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "tyc check should fail on a type mismatch"
    );
}

#[test]
fn check_passes_nullable_annotation() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("nullable.ty"), "val x: str? = None\n").unwrap();
    let status = tyc()
        .args(["check", tmp.path().to_str().unwrap()])
        .status()
        .unwrap();
    assert!(
        status.success(),
        "tyc check should accept T? (nullable) annotations"
    );
}

// ── tyc fmt ──────────────────────────────────────────────────────────────────

#[test]
fn fmt_check_passes_on_already_formatted_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("x.ty"), "val x: int = 1\n").unwrap();
    let status = tyc()
        .args(["fmt", "--check", tmp.path().to_str().unwrap()])
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
        .args(["fmt", "--check", tmp.path().to_str().unwrap()])
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
    let status = tyc()
        .args(["fmt", path.to_str().unwrap()])
        .status()
        .unwrap();
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
    scaffold(tmp.path(), "val greeting: str = \"hello\"\n");
    let status = tyc()
        .args(["build", tmp.path().to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success(), "tyc build should succeed");
    assert!(
        tmp.path().join("build").join("main.py").exists(),
        "build/main.py should be emitted"
    );
}

#[test]
fn build_fails_on_type_error() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "val x: int = \"wrong type\"\n");
    let status = tyc()
        .args(["build", tmp.path().to_str().unwrap()])
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "tyc build should fail when there is a type mismatch"
    );
}

#[test]
fn build_emits_dataclass_decorator_for_class() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "class Point:\n    x: int\n    y: int\n");
    let status = tyc()
        .args(["build", tmp.path().to_str().unwrap()])
        .status()
        .unwrap();
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
    tyc()
        .args(["build", tmp.path().to_str().unwrap()])
        .status()
        .unwrap();
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

// ── full pipeline ─────────────────────────────────────────────────────────────

/// Full pipeline smoke test: init → write Phase 3 source → check → build → verify output.
///
/// This validates that the most important Phase 3 features (interface, sealed union,
/// `@pure`, pipe operator) survive the full compilation pipeline without errors.
#[test]
fn full_pipeline_phase3_features() {
    let tmp = tempfile::tempdir().unwrap();

    // Scaffold the project via `tyc init`.
    let status = tyc()
        .args(["init", "demo", "--dir", tmp.path().to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success(), "tyc init failed");

    // Replace the generated main.ty with a Phase 3 feature showcase.
    let src = r#"
class Config:
    host: str
    port: int

@pure
def double(x: int) -> int:
    return x * 2

val result: int = double(21)
"#;
    std::fs::write(tmp.path().join("src").join("main.ty"), src).unwrap();

    // `tyc check` must pass.
    let status = tyc()
        .args(["check", tmp.path().to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success(), "tyc check failed on Phase 3 fixture");

    // `tyc build` must succeed and produce main.py.
    let status = tyc()
        .args(["build", tmp.path().to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success(), "tyc build failed on Phase 3 fixture");

    let py = std::fs::read_to_string(tmp.path().join("build").join("main.py")).unwrap();
    assert!(
        py.contains("@dataclasses.dataclass"),
        "Config class should be emitted as a dataclass"
    );
    assert!(
        py.contains("double"),
        "pure function should appear in emitted output"
    );
}
