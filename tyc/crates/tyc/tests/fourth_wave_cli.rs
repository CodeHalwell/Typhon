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

// ── `tyc lsp` transport (W4-11) ─────────────────────────────────────────────

mod lsp_transport {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::{Child, ChildStdin, Command, Stdio};
    use std::sync::mpsc::{channel, Receiver};
    use std::time::Duration;

    struct Server {
        child: Child,
        stdin: Option<ChildStdin>,
        frames: Receiver<serde_json::Value>,
    }

    impl Server {
        fn start() -> Self {
            let mut child = Command::new(env!("CARGO_BIN_EXE_tyc"))
                .args(["lsp", "--log-level", "error"])
                .env("TYC_NO_INTROSPECT", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let stdin = child.stdin.take();
            let stdout = child.stdout.take().unwrap();
            let (tx, frames) = channel();
            std::thread::spawn(move || {
                let mut r = BufReader::new(stdout);
                loop {
                    let mut len = None;
                    loop {
                        let mut line = String::new();
                        if r.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        let line = line.trim_end();
                        if line.is_empty() {
                            break;
                        }
                        if let Some(v) = line.strip_prefix("Content-Length:") {
                            len = v.trim().parse::<usize>().ok();
                        }
                    }
                    let mut body = vec![0u8; len.unwrap()];
                    if r.read_exact(&mut body).is_err() {
                        return;
                    }
                    if tx.send(serde_json::from_slice(&body).unwrap()).is_err() {
                        return;
                    }
                }
            });
            Server {
                child,
                stdin,
                frames,
            }
        }

        fn raw(&mut self, bytes: &[u8]) {
            let w = self.stdin.as_mut().unwrap();
            w.write_all(bytes).unwrap();
            w.flush().unwrap();
        }

        fn frame(&mut self, body: &[u8]) {
            let mut msg = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
            msg.extend_from_slice(body);
            self.raw(&msg);
        }

        fn send(&mut self, v: serde_json::Value) {
            self.frame(&serde_json::to_vec(&v).unwrap());
        }

        /// The next message that satisfies `pred`, within 20 s.
        fn expect(&self, pred: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
            loop {
                let msg = self
                    .frames
                    .recv_timeout(Duration::from_secs(20))
                    .expect("server went quiet (or exited)");
                if pred(&msg) {
                    return msg;
                }
            }
        }

        fn initialize(&mut self, id: i64) -> serde_json::Value {
            self.send(
                serde_json::json!({"jsonrpc":"2.0","id":id,"method":"initialize",
                "params":{"capabilities":{}}}),
            );
            self.expect(|m| m["id"] == id)
        }

        fn exit_code(mut self) -> i32 {
            for _ in 0..200 {
                if let Some(status) = self.child.try_wait().unwrap() {
                    return status.code().unwrap_or(-1);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = self.child.kill();
            panic!("server did not exit");
        }
    }

    /// W4-11: a malformed frame gets an error reply and the server keeps
    /// serving. tower-lsp used to stop reading after the first one and exit 0.
    #[test]
    fn malformed_frames_get_an_error_and_the_server_survives() {
        let mut s = Server::start();
        let cases: [(&[u8], i64); 5] = [
            (b"{\"jsonrpc\":\"2.0\",\"id\":1,\"meth", -32700),
            (b"not json at all", -32700),
            (b"[1, 2, 3]", -32600),
            (
                b"{\"id\":2,\"method\":\"initialize\",\"params\":{}}",
                -32600,
            ),
            (b"{\"jsonrpc\":\"2.0\",\"method\":\"x\xff\xfe\"}", -32700),
        ];
        for (body, code) in cases {
            s.frame(body);
            let reply = s.expect(|m| m.get("error").is_some());
            assert_eq!(
                reply["error"]["code"].as_i64(),
                Some(code),
                "{} → {reply}",
                String::from_utf8_lossy(body)
            );
        }
        // A header block without `Content-Length` is skipped the same way.
        s.raw(b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n");
        let reply = s.expect(|m| m.get("error").is_some());
        assert_eq!(reply["error"]["code"].as_i64(), Some(-32700), "{reply}");

        let init = s.initialize(7);
        assert!(init["result"]["capabilities"].is_object(), "{init}");
        s.send(serde_json::json!({"jsonrpc":"2.0","id":8,"method":"shutdown"}));
        s.expect(|m| m["id"] == 8);
        s.send(serde_json::json!({"jsonrpc":"2.0","method":"exit"}));
        assert_eq!(s.exit_code(), 0);
    }

    /// W4-11: `exit` without a preceding `shutdown` exits 1, per the spec.
    #[test]
    fn exit_without_shutdown_is_exit_code_one() {
        let mut s = Server::start();
        s.initialize(1);
        s.send(serde_json::json!({"jsonrpc":"2.0","method":"exit"}));
        assert_eq!(s.exit_code(), 1);
    }

    /// W4-11: stdin closing before `shutdown` is not a clean exit either.
    #[test]
    fn stdin_closing_without_shutdown_is_not_exit_code_zero() {
        let mut s = Server::start();
        s.initialize(1);
        s.stdin = None;
        assert_eq!(s.exit_code(), 1);
    }
}

// ── reserved `typhon_runtime` (W4-12) ───────────────────────────────────────

/// W4-12: a project module named `typhon_runtime` is replaced by the
/// generated runtime whenever the program needs it. When the program imports
/// a name of its own from it, the build used to succeed and the program then
/// failed with `ImportError: cannot import name 'helper'`; it now fails to
/// build with `tyc::reserved_module_name`, and `tyc check` warns.
#[test]
fn a_user_typhon_runtime_that_the_runtime_would_replace_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    project(
        dir,
        "rt",
        &[
            (
                "typhon_runtime/__init__.ty",
                "def helper() -> int:\n    return 41\n",
            ),
            (
                "main.ty",
                "from typhon_runtime import helper\n\ndef f() -> Result[int, str]:\n    return Ok(helper() + 1)\n\nprint(f())\n",
            ),
        ],
    );
    let check = tyc()
        .current_dir(dir)
        .args(["check", "src"])
        .output()
        .unwrap();
    assert!(check.status.success(), "{}", text(&check));
    assert!(
        text(&check).contains("tyc::reserved_module_name"),
        "{}",
        text(&check)
    );

    let build = tyc()
        .current_dir(dir)
        .args(["build", "--no-sync"])
        .output()
        .unwrap();
    assert!(!build.status.success(), "{}", text(&build));
    let t = text(&build);
    assert!(t.contains("tyc::reserved_module_name"), "{t}");
    assert!(t.contains("helper"), "{t}");
}

/// W4-12: a `typhon_runtime` module the generated runtime never replaces (no
/// `.ty` file uses a runtime feature or imports it — importing it is itself
/// what pulls the generated runtime in) keeps building, with a warning.
#[test]
fn a_user_typhon_runtime_the_build_never_replaces_still_builds() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    project(
        dir,
        "rt",
        &[
            ("typhon_runtime.ty", "def helper() -> int:\n    return 41\n"),
            ("main.ty", "print(1 + 1)\n"),
        ],
    );
    let build = tyc()
        .current_dir(dir)
        .args(["build", "--no-sync"])
        .output()
        .unwrap();
    assert!(build.status.success(), "{}", text(&build));
    assert!(
        text(&build).contains("tyc::reserved_module_name"),
        "{}",
        text(&build)
    );
    assert!(dir.join("build/typhon_runtime.py").exists());
    assert!(!dir.join("build/typhon_runtime").exists());
}

// ── `tyc fmt` symlink write-through (W4-13, fmt half) ───────────────────────

/// W4-13: `tyc fmt src/` where `src` is a symlink leaving the project used to
/// reformat the files at the link's target. `tyc fmt .` already skipped it.
#[cfg(unix)]
#[test]
fn fmt_does_not_write_through_a_src_symlink_leaving_the_project() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("proj");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(proj.join("typhon.toml"), "[project]\nname = \"p\"\n").unwrap();
    let victim = "def f():\n\tpass\n";
    std::fs::write(outside.join("victim.ty"), victim).unwrap();
    std::os::unix::fs::symlink(&outside, proj.join("src")).unwrap();

    // Even when the link's target carries its own `typhon.toml`.
    std::fs::write(outside.join("typhon.toml"), "[project]\nname = \"o\"\n").unwrap();
    for arg in ["src", "src/", "src/victim.ty"] {
        let out = tyc()
            .current_dir(&proj)
            .args(["fmt", arg])
            .output()
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(outside.join("victim.ty")).unwrap(),
            victim,
            "`tyc fmt {arg}` wrote outside the project: {}",
            text(&out)
        );
        assert!(text(&out).contains("outside the project"), "{}", text(&out));
    }

    // A symlink that stays inside the project is still followed.
    std::fs::remove_file(proj.join("src")).unwrap();
    std::fs::create_dir_all(proj.join("code")).unwrap();
    std::fs::write(proj.join("code/inside.ty"), victim).unwrap();
    std::os::unix::fs::symlink(proj.join("code"), proj.join("src")).unwrap();
    let out = tyc()
        .current_dir(&proj)
        .args(["fmt", "src"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert_ne!(
        std::fs::read_to_string(proj.join("code/inside.ty")).unwrap(),
        victim
    );
}

// ── smaller CLI issues (W4-15) ───────────────────────────────────────────────

/// W4-15: `tyc check` on a path with nothing to check exits non-zero — an
/// empty directory or a `.py`-only path used to pass, so CI went green on a
/// mistyped path.
#[test]
fn check_with_nothing_to_check_fails() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("script.py"), "print(1)\n").unwrap();
    let out = tyc().arg("check").arg(tmp.path()).output().unwrap();
    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("no checkable files"), "{}", text(&out));
    let out = tyc()
        .arg("check")
        .arg(tmp.path().join("does-not-exist"))
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", text(&out));
}

/// W4-15: a piped `tyc repl` exits non-zero when a snippet fails to compile
/// or raises; it used to exit 0.
#[test]
fn piped_repl_exit_code_reflects_failed_snippets() {
    use std::io::Write;
    let run = |input: &str| {
        let mut child = tyc()
            .arg("repl")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    let ok = run("print(1 + 1)\n");
    if text(&ok).contains("no Python interpreter found") {
        eprintln!("skipping: no python on PATH");
        return;
    }
    assert!(ok.status.success(), "{}", text(&ok));
    let compile_error = run("print(1)\nlet x: int = \"s\"\nprint(2)\n");
    assert!(!compile_error.status.success(), "{}", text(&compile_error));
    assert!(text(&compile_error).contains("1 snippet failed"));
    let raised = run("raise ValueError(\"boom\")\n");
    assert!(!raised.status.success(), "{}", text(&raised));
}
