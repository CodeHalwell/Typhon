//! End-to-end regression tests for the 2026-10-03 review's W4 workstream
//! (CLI commands, filesystem safety, config loading). Every test builds its
//! project inside a disposable `tempfile::tempdir()`.

use std::path::Path;
use std::process::{Command, Output};

fn tyc() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tyc"));
    c.env("TYC_NO_SYNC", "1")
        .env("TYC_NO_INTROSPECT", "1")
        .env("NO_COLOR", "1");
    c
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A minimal project at `dir` with `files` (relative to `dir/src`).
fn project(dir: &Path, name: &str, files: &[(&str, &str)]) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("typhon.toml"),
        format!("[project]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
    )
    .unwrap();
    for (rel, body) in files {
        let path = dir.join("src").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
}

/// W4-06: `tyc check <dir>` over a directory holding several projects checks
/// each project against its own modules. Two apps that each define a
/// `models.Position` with different fields used to see each other's class.
#[test]
fn check_keeps_nested_projects_apart() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    project(
        &root.join("a"),
        "a",
        &[
            ("models.ty", "class Position:\n    x: int\n    y: int\n"),
            (
                "main.ty",
                "from models import Position\nlet p: Position = Position(x=1, y=2)\nprint(p.x)\n",
            ),
        ],
    );
    project(
        &root.join("b"),
        "b",
        &[
            ("models.ty", "class Position:\n    symbol: str\n    qty: int\n"),
            (
                "main.ty",
                "from models import Position\nlet p: Position = Position(symbol=\"s\", qty=2)\nprint(p.qty)\n",
            ),
        ],
    );
    // A loose script beside the projects is still checked on its own.
    std::fs::write(root.join("loose.ty"), "let n: int = 1\nprint(n)\n").unwrap();

    let out = tyc()
        .current_dir(root)
        .args(["check", "."])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("checked 5 file(s)"), "{}", text(&out));

    // An error inside one project is still reported, against that project.
    std::fs::write(
        root.join("b/src/main.ty"),
        "from models import Position\nlet p: Position = Position(x=1, y=2)\n",
    )
    .unwrap();
    let out = tyc()
        .current_dir(root)
        .args(["check", "."])
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("tyc::unknown_kwarg"), "{}", text(&out));
    assert!(text(&out).contains("b/src/main.ty"), "{}", text(&out));
    assert!(!text(&out).contains("a/src/main.ty"), "{}", text(&out));
}

/// W4-06: a project's own `typhon.toml` governs its files even when the check
/// starts from a parent directory — here `[strictness] unused-import =
/// "error"` in one app must not leak into its sibling.
#[test]
fn check_uses_each_nested_projects_config() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    project(
        &root.join("strict"),
        "strict",
        &[("main.ty", "import os\n")],
    );
    std::fs::write(
        root.join("strict/typhon.toml"),
        "[project]\nname = \"strict\"\n[strictness]\nunused-import = \"error\"\n",
    )
    .unwrap();
    project(&root.join("lax"), "lax", &[("main.ty", "import os\n")]);

    let out = tyc()
        .current_dir(root)
        .args(["check", "."])
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", text(&out));
    let t = text(&out);
    assert!(t.contains("1 error"), "{t}");
    assert!(t.contains("strict/src/main.ty"), "{t}");
}
