//! VM ↔ CPython parity harness for `examples/parity/`.
//!
//! `docs/vm.md` states the tree-walking VM behind `tyc run` "must stay a
//! drop-in for `tyc build && python` — VM/CPython divergences are bugs". This
//! test enforces that on a corpus written to exercise the value semantics most
//! likely to drift: numeric edge cases, float and container reprs, dunder
//! protocols, generators, pattern matching, comprehensions, and the lowering
//! of Typhon-specific forms.
//!
//! Two directories, two opposite assertions:
//!
//! * `examples/parity/*.ty` — must produce **byte-identical stdout** under
//!   `tyc run` and `tyc build` + CPython.
//! * `examples/parity/divergent/*.ty` — confirmed divergences, each documented
//!   in a `# DIVERGENT:` header. Asserted to **still** differ, so fixing one
//!   fails the test as a prompt to move the file up a level.
//!
//! Skipped when no CPython ≥3.13 is on PATH, since the emitted code targets
//! 3.13+ syntax.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repo root above tyc/crates/tyc")
        .to_path_buf()
}

fn parity_dir() -> PathBuf {
    repo_root().join("examples").join("parity")
}

/// A CPython ≥3.13 interpreter, or `None` if the host has none.
fn cpython() -> Option<String> {
    for candidate in ["python3.15", "python3.14", "python3.13", "python3"] {
        let out = Command::new(candidate)
            .args(["-c", "import sys; print(sys.version_info >= (3, 13))"])
            .output();
        if let Ok(out) = out {
            if String::from_utf8_lossy(&out.stdout).trim() == "True" {
                return Some(candidate.to_string());
            }
        }
    }
    None
}

fn ty_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .filter_map(|e| {
            let p = e.ok()?.path();
            (p.extension()? == "ty").then_some(p)
        })
        .collect();
    files.sort();
    files
}

struct Run {
    ok: bool,
    stdout: String,
    detail: String,
}

fn run_vm(path: &Path) -> Run {
    let out = Command::new(env!("CARGO_BIN_EXE_tyc"))
        .arg("run")
        .arg(path)
        .output()
        .unwrap();
    Run {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        detail: String::from_utf8_lossy(&out.stderr).to_string(),
    }
}

fn run_compiled(path: &Path, python: &str) -> Run {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::copy(path, tmp.path().join("src").join("main.ty")).unwrap();
    std::fs::write(
        tmp.path().join("typhon.toml"),
        "[project]\nname = \"parity\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
         [python]\ntarget = \"3.13\"\n",
    )
    .unwrap();

    let build = Command::new(env!("CARGO_BIN_EXE_tyc"))
        .args(["build", "--no-sync"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    if !build.status.success() {
        return Run {
            ok: false,
            stdout: String::new(),
            detail: format!(
                "tyc build failed:\n{}{}",
                String::from_utf8_lossy(&build.stdout),
                String::from_utf8_lossy(&build.stderr)
            ),
        };
    }

    let out = Command::new(python)
        .arg("build/main.py")
        .current_dir(tmp.path())
        .output()
        .unwrap();
    Run {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        detail: String::from_utf8_lossy(&out.stderr).to_string(),
    }
}

#[test]
fn vm_output_matches_cpython() {
    let Some(python) = cpython() else {
        eprintln!("skipping parity sweep: no CPython >= 3.13 on PATH");
        return;
    };

    let files = ty_files(&parity_dir());
    assert!(
        files.len() >= 10,
        "expected the parity corpus, found {}",
        files.len()
    );

    let mut failures = Vec::new();
    for path in &files {
        let rel = path
            .strip_prefix(repo_root())
            .unwrap()
            .display()
            .to_string();
        let compiled = run_compiled(path, &python);
        if !compiled.ok {
            failures.push(format!("{rel}: compiled run failed:\n{}", compiled.detail));
            continue;
        }
        let vm = run_vm(path);
        if !vm.ok {
            failures.push(format!("{rel}: `tyc run` failed:\n{}", vm.detail));
            continue;
        }
        if vm.stdout != compiled.stdout {
            failures.push(format!(
                "{rel}: VM output differs from CPython\n--- VM ---\n{}\n--- CPython ---\n{}",
                vm.stdout, compiled.stdout
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} parity example(s) diverged between `tyc run` and `tyc build` + CPython.\n\
         The VM is a drop-in for the compiled path, so each of these is a bug — fix it, or \
         (if the divergence is confirmed and not yet fixable) move the file to \
         examples/parity/divergent/ with a `# DIVERGENT:` header.\n\n{}",
        failures.len(),
        files.len(),
        failures.join("\n\n")
    );
}

/// The confirmed-divergence tripwire. These files are checked in *because*
/// they misbehave; when one starts agreeing, promote it out of `divergent/`.
#[test]
fn documented_divergences_still_diverge() {
    let Some(python) = cpython() else {
        eprintln!("skipping divergence sweep: no CPython >= 3.13 on PATH");
        return;
    };

    let dir = parity_dir().join("divergent");
    let files = ty_files(&dir);
    assert!(!files.is_empty(), "divergent corpus is empty");

    let mut agreed = Vec::new();
    for path in &files {
        let source = std::fs::read_to_string(path).unwrap();
        assert!(
            source.starts_with("# DIVERGENT:"),
            "{} must open with a `# DIVERGENT:` header explaining the divergence",
            path.display()
        );

        let compiled = run_compiled(path, &python);
        let vm = run_vm(path);
        // A divergence counts as resolved only when both paths succeed *and*
        // agree; a VM crash is still a divergence.
        if compiled.ok && vm.ok && compiled.stdout == vm.stdout {
            agreed.push(
                path.strip_prefix(repo_root())
                    .unwrap()
                    .display()
                    .to_string(),
            );
        }
    }

    assert!(
        agreed.is_empty(),
        "a documented VM/CPython divergence has been fixed — move these out of \
         examples/parity/divergent/ into examples/parity/ and drop the `# DIVERGENT:` \
         header:\n  {}",
        agreed.join("\n  ")
    );
}
