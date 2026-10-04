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
