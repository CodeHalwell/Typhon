//! `tyc repl` — interactive Typhon evaluator.
//!
//! Reads `.ty` source one line (or one blank-line-terminated block) at a
//! time, accumulates the session, recompiles via the full Typhon pipeline,
//! and executes the result with a Python interpreter. Only the *new* tail
//! of stdout is shown after each input, so earlier `print(...)` calls are
//! not re-displayed every iteration.
//!
//! Limitations:
//!   - The REPL recompiles the entire session each evaluation; large
//!     sessions get slower. Acceptable for v1 exploratory use.
//!   - Expression statements are *not* auto-printed; the user must call
//!     `print(...)` explicitly. This matches Python's `python -c` and
//!     keeps the REPL identical in behaviour to `tyc build` + run.
//!   - There is no readline support; arrow keys insert escape sequences.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use clap::Args;
use miette::{miette, Result};

use tyc_db::{check_file, TycDatabase};
use tyc_desugar::{desugar_module_with, DesugarOptions};
use tyc_emit::emit_with_line_offsets;
use tyc_syntax::preprocess::{
    expand_gather_blocks, expand_go_calls, expand_lazy_imports, expand_pipes, expand_question_ops,
    expand_with_chains, preprocess,
};

/// Arguments for `tyc repl`.
#[derive(Args, Debug)]
pub struct ReplArgs {
    /// Python interpreter to use (defaults to `python3`).
    #[arg(long, value_name = "PATH", default_value = "python3")]
    pub python: String,

    /// Pre-load this `.ty` file as the initial session before prompting.
    #[arg(long, value_name = "FILE")]
    pub load: Option<PathBuf>,
}

pub fn run(args: ReplArgs) -> Result<()> {
    let mut session = ReplSession::new(args.python.clone());

    if let Some(path) = args.load.as_deref() {
        let src = std::fs::read_to_string(path)
            .map_err(|e| miette!("cannot read '{}': {e}", path.display()))?;
        session.feed_block(&src)?;
        // Run once after loading so any pre-existing prints surface.
        if let Some(new_output) = session.evaluate()? {
            print!("{new_output}");
        }
    }

    println!(
        "tyc repl — Typhon {} on {}\nType `:quit` to exit, `:reset` to clear, `:show` to dump the session.",
        env!("CARGO_PKG_VERSION"),
        args.python
    );

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut stdin = stdin.lock();
    let mut stdout = stdout.lock();

    let mut buf = String::new();
    loop {
        write!(stdout, ">>> ").ok();
        stdout.flush().ok();
        buf.clear();
        let n = stdin
            .read_line(&mut buf)
            .map_err(|e| miette!("read failed: {e}"))?;
        if n == 0 {
            // EOF — exit cleanly.
            writeln!(stdout).ok();
            break;
        }
        let line = buf.trim_end_matches(['\r', '\n']);
        match line.trim() {
            ":quit" | ":q" | ":exit" => break,
            ":reset" => {
                session.reset();
                writeln!(stdout, "session reset.").ok();
                continue;
            }
            ":show" => {
                writeln!(stdout, "{}", session.source()).ok();
                continue;
            }
            "" => continue,
            _ => {}
        }

        // Multi-line: if the line ends with `:` or `\`, keep reading until a
        // blank line. Keeps `def`/`class` blocks practical.
        let mut block = String::from(line);
        block.push('\n');
        if needs_continuation(line) {
            loop {
                write!(stdout, "... ").ok();
                stdout.flush().ok();
                buf.clear();
                let n = stdin
                    .read_line(&mut buf)
                    .map_err(|e| miette!("read failed: {e}"))?;
                if n == 0 {
                    break;
                }
                if buf.trim().is_empty() {
                    break;
                }
                block.push_str(buf.trim_end_matches(['\r', '\n']));
                block.push('\n');
            }
        }

        // Try to compile; on error, roll back and report.
        match session.feed_block(&block) {
            Ok(()) => match session.evaluate() {
                Ok(Some(out)) => {
                    if !out.is_empty() {
                        print!("{out}");
                        if !out.ends_with('\n') {
                            println!();
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => eprintln!("runtime: {e}"),
            },
            Err(e) => eprintln!("error: {e}"),
        }
    }

    Ok(())
}

fn needs_continuation(line: &str) -> bool {
    let trimmed = line.trim_end();
    trimmed.ends_with(':') || trimmed.ends_with('\\')
}

// ── session state ────────────────────────────────────────────────────────────

/// Holds the accumulated `.ty` source and the last stdout we observed, so we
/// can emit only the new tail after each evaluation.
struct ReplSession {
    python: String,
    blocks: Vec<String>,
    last_stdout_len: usize,
}

impl ReplSession {
    fn new(python: String) -> Self {
        Self {
            python,
            blocks: Vec::new(),
            last_stdout_len: 0,
        }
    }

    fn reset(&mut self) {
        self.blocks.clear();
        self.last_stdout_len = 0;
    }

    fn source(&self) -> String {
        self.blocks.concat()
    }

    /// Type-check `block` in the *context* of the current session before
    /// committing it. Rejecting bad blocks here keeps the cumulative source
    /// from getting wedged.
    fn feed_block(&mut self, block: &str) -> Result<()> {
        let mut trial = self.source();
        if !trial.ends_with('\n') {
            trial.push('\n');
        }
        trial.push_str(block);
        compile_to_python(&trial)?;
        self.blocks.push(if block.ends_with('\n') {
            block.to_owned()
        } else {
            format!("{block}\n")
        });
        Ok(())
    }

    /// Compile the cumulative session, run it under the configured Python,
    /// and return the *new* stdout tail (None if nothing was produced).
    fn evaluate(&mut self) -> Result<Option<String>> {
        let src = self.source();
        if src.trim().is_empty() {
            return Ok(None);
        }
        let py = compile_to_python(&src)?;

        let tmp = tempfile::Builder::new()
            .prefix("tyc-repl-")
            .suffix(".py")
            .tempfile()
            .map_err(|e| miette!("cannot create temp file: {e}"))?;
        std::fs::write(tmp.path(), &py)
            .map_err(|e| miette!("cannot write temp file: {e}"))?;

        let output = Command::new(&self.python)
            .arg(tmp.path())
            .stdin(Stdio::null())
            .output()
            .map_err(|e| miette!("cannot spawn '{}': {e}", self.python))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(miette!(
                "{} exited with {}: {}",
                self.python,
                output.status,
                stderr.trim()
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let new_tail = if stdout.len() > self.last_stdout_len {
            stdout[self.last_stdout_len..].to_owned()
        } else {
            String::new()
        };
        self.last_stdout_len = stdout.len();
        Ok(Some(new_tail))
    }
}

// ── compile pipeline ────────────────────────────────────────────────────────

/// Run the full preprocessing + check + desugar + emit pipeline on `source`
/// and return the emitted Python text. Diagnostics are surfaced as miette
/// errors with a one-line summary; the full set is dropped (the REPL is for
/// interactive use, not CI).
pub(crate) fn compile_to_python(source: &str) -> Result<String> {
    let expanded = expand_question_ops(&expand_pipes(&expand_with_chains(&expand_go_calls(
        &expand_gather_blocks(&expand_lazy_imports(source)),
    ))));
    let prep = preprocess(&expanded);

    let mut db = TycDatabase::new();
    let diags = check_file(&mut db, "<repl>".to_owned(), prep.python_source.clone());
    if diags.has_errors() {
        let first = diags
            .errors()
            .first()
            .map(|d| format!("{d}"))
            .unwrap_or_else(|| "type error".into());
        return Err(miette!("{first}"));
    }

    let module = tyc_syntax::parse_module(&prep.python_source)
        .map(|p| p.into_syntax())
        .map_err(|e| miette!("parse error: {e}"))?;

    let desugar = desugar_module_with(&module, DesugarOptions::default());
    let (py, _offsets) = emit_with_line_offsets(&desugar.module);
    Ok(crate::commands::build::strip_mutability_keywords(&py))
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_continuation_after_colon() {
        assert!(needs_continuation("def f():"));
        assert!(needs_continuation("if x:   "));
        assert!(!needs_continuation("let x = 1"));
    }

    #[test]
    fn needs_continuation_after_backslash() {
        assert!(needs_continuation("let x = 1 + \\"));
    }

    #[test]
    fn compile_simple_assignment_returns_python() {
        let py = compile_to_python("let x: int = 42\n").expect("should compile");
        assert!(py.contains("x"), "emitted python should mention `x`: {py}");
        assert!(py.contains("42"), "emitted python should mention `42`: {py}");
    }

    #[test]
    fn compile_type_error_is_reported() {
        let err = compile_to_python("let x: int = \"oops\"\n").expect_err("should fail");
        let msg = format!("{err:?}");
        assert!(
            msg.to_lowercase().contains("type") || msg.to_lowercase().contains("mismatch"),
            "expected a type error message, got: {msg}"
        );
    }

    #[test]
    fn session_rejects_bad_block_and_keeps_state() {
        let mut s = ReplSession::new("python3".into());
        s.feed_block("let x: int = 1\n").expect("good block accepted");
        let err = s.feed_block("let y: int = \"nope\"\n");
        assert!(err.is_err(), "bad block must be rejected");
        // The good block is still in the session.
        assert!(s.source().contains("x: int = 1"));
        assert!(!s.source().contains("nope"), "bad block must not be committed");
    }

    #[test]
    fn session_reset_clears_state() {
        let mut s = ReplSession::new("python3".into());
        s.feed_block("let x: int = 1\n").unwrap();
        s.last_stdout_len = 12;
        s.reset();
        assert!(s.source().is_empty());
        assert_eq!(s.last_stdout_len, 0);
    }
}
