//! Regression harness for the *working* half of `examples/`.
//!
//! `CLAUDE.md` calls the example corpus "the regression net", but until this
//! test existed nothing enforced it — a type-checker change could break every
//! app in `examples/apps/` and CI would stay green.
//!
//! This asserts the two properties the corpus claims:
//!
//! 1. Every single-file exercise and every multi-file app under `examples/`
//!    is `tyc check`-clean, with no diagnostics at any severity.
//! 2. The checked-in `.py` companion beside each single-file exercise is
//!    byte-identical to what `tyc build` emits today, so the "here is exactly
//!    what this lowers to" promise in `examples/README.md` holds.
//!
//! `examples/errors/` is deliberately excluded — it has its own harness in
//! `error_examples.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repo root above tyc/crates/tyc")
        .to_path_buf()
}

fn tyc() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tyc"))
}

fn combined(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Single-file exercises: `examples/NN-topic/name.ty`, excluding the multi-file
/// `47-mini-app` and the `errors/` corpus.
fn single_file_exercises() -> Vec<PathBuf> {
    let examples = repo_root().join("examples");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&examples).unwrap() {
        let dir = entry.unwrap().path();
        if !dir.is_dir() {
            continue;
        }
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        if name == "errors" || name == "apps" || name == "47-mini-app" {
            continue;
        }
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "ty") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// Every directory under `examples/` that owns a `typhon.toml`.
fn projects() -> Vec<PathBuf> {
    let examples = repo_root().join("examples");
    let mut dirs = Vec::new();
    for root in [examples.clone(), examples.join("apps")] {
        for entry in std::fs::read_dir(&root).unwrap() {
            let dir = entry.unwrap().path();
            if dir.is_dir() && dir.join("typhon.toml").is_file() {
                dirs.push(dir);
            }
        }
    }
    dirs.sort();
    dirs
}

#[test]
fn every_single_file_example_checks_clean() {
    let files = single_file_exercises();
    assert!(
        files.len() >= 30,
        "expected the full exercise corpus, found {}",
        files.len()
    );

    let mut failures = Vec::new();
    for path in &files {
        let out = tyc().arg("check").arg(path).output().unwrap();
        let text = combined(&out);
        if !out.status.success() || text.contains("tyc::") {
            failures.push(format!(
                "{}\n{}",
                path.strip_prefix(repo_root()).unwrap().display(),
                text.trim()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} single-file example(s) are not check-clean:\n\n{}",
        failures.len(),
        files.len(),
        failures.join("\n\n")
    );
}

#[test]
fn every_example_project_checks_clean() {
    let dirs = projects();
    assert!(
        dirs.len() >= 15,
        "expected the app corpus, found {}",
        dirs.len()
    );

    let mut failures = Vec::new();
    for dir in &dirs {
        let out = tyc()
            .args(["check", "src/"])
            .current_dir(dir)
            .output()
            .unwrap();
        let text = combined(&out);
        if !out.status.success() || text.contains("tyc::") {
            failures.push(format!(
                "{}\n{}",
                dir.strip_prefix(repo_root()).unwrap().display(),
                text.trim()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} example project(s) are not check-clean:\n\n{}",
        failures.len(),
        dirs.len(),
        failures.join("\n\n")
    );
}

/// `examples/README.md` promises the checked-in `.py` beside each `.ty` shows
/// "exactly what each Typhon construct lowers to". Emission changes silently
/// falsify that; this catches the drift.
#[test]
fn checked_in_py_companions_match_a_fresh_build() {
    let mut drifted = Vec::new();
    let mut compared = 0usize;

    for ty_path in single_file_exercises() {
        let py_path = ty_path.with_extension("py");
        let Ok(expected) = std::fs::read_to_string(&py_path) else {
            continue; // not every exercise ships a companion
        };

        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::copy(&ty_path, tmp.path().join("src").join("main.ty")).unwrap();
        std::fs::write(
            tmp.path().join("typhon.toml"),
            "[project]\nname = \"example\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
             [python]\ntarget = \"3.13\"\n",
        )
        .unwrap();

        let out = tyc()
            .args(["build", "--no-sync"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "tyc build failed for {}:\n{}",
            ty_path.display(),
            combined(&out)
        );

        let actual = std::fs::read_to_string(tmp.path().join("build").join("main.py")).unwrap();
        compared += 1;
        if actual != expected {
            drifted.push(
                py_path
                    .strip_prefix(repo_root())
                    .unwrap()
                    .display()
                    .to_string(),
            );
        }
    }

    assert!(
        compared >= 30,
        "expected ~31 companions, compared {compared}"
    );
    assert!(
        drifted.is_empty(),
        "the checked-in `.py` companion no longer matches `tyc build` output — \
         rebuild and commit these {} file(s):\n  {}",
        drifted.len(),
        drifted.join("\n  ")
    );
}
