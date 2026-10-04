//! End-to-end regression tests for the 2026-10-03 review's W3 cross-module
//! items: a project is checked, built and run on CPython, and run on the VM,
//! and every surface must agree.

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

fn scaffold(dir: &Path, files: &[(&str, &str)]) {
    std::fs::write(
        dir.join("typhon.toml"),
        "[project]\nname = \"w3\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
         [python]\ntarget = \"3.13\"\n[emit]\nformat = false\n[strictness]\n[env]\n",
    )
    .unwrap();
    for (rel, text) in files {
        let path = dir.join("src").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

fn run_tyc(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = tyc().current_dir(dir).args(args).output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `tyc check`, `tyc build` + CPython and `tyc run` all succeed and both
/// runs print `expected`.
fn assert_all_surfaces_print(files: &[(&str, &str)], expected: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    scaffold(dir, files);
    let (ok, out, err) = run_tyc(dir, &["check", "src"]);
    assert!(ok, "tyc check failed:\n{out}{err}");
    let (ok, out, err) = run_tyc(dir, &["build"]);
    assert!(ok, "tyc build failed:\n{out}{err}");
    if let Some(py) = python() {
        let run = Command::new(py)
            .current_dir(dir.join("build"))
            .arg("main.py")
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            expected,
            "CPython output; stderr:\n{}",
            String::from_utf8_lossy(&run.stderr)
        );
    }
    let (ok, out, err) = run_tyc(dir, &["run"]);
    assert!(ok, "tyc run failed:\n{out}{err}");
    assert_eq!(out, expected, "VM output; stderr:\n{err}");
}

// ── W3-02: `extend BUILTIN` through a `pub *` facade ───────────────────────

#[test]
fn builtin_extensions_travel_through_pub_star_facades() {
    assert_all_surfaces_print(
        &[
            ("pkg/__init__.ty", "pub *\n"),
            (
                "pkg/text.ty",
                "extend str:\n    def slug(self) -> str:\n        return self.lower().replace(\" \", \"-\")\n\n\
                 pub def describe(s: str) -> str:\n    return s.slug()\n",
            ),
            ("pkg/sub/__init__.ty", "pub *\n"),
            (
                "pkg/sub/nums.ty",
                "extend int:\n    def double(self) -> int:\n        return self * 2\n\n\
                 pub def seven() -> int:\n    return 7\n",
            ),
            (
                "main.ty",
                "import pkg\nfrom pkg import describe, seven\n\n\
                 def main() -> None:\n\
                 \x20   let s: str = \"Hello World\"\n\
                 \x20   print(describe(s))\n\
                 \x20   print(s.slug())\n\
                 \x20   print(describe(s).slug())\n\
                 \x20   let n: int = seven()\n\
                 \x20   print(n.double())\n\
                 \x20   print(pkg.describe(\"A B\").slug())\n\n\
                 main()\n",
            ),
        ],
        "hello-world\nhello-world\nhello-world\n14\na-b\n",
    );
}
