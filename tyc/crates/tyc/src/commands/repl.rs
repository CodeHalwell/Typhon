//! `tyc repl` — interactive Typhon evaluator.
//!
//! Reads `.ty` source one line (or one blank-line-terminated block) at a
//! time, accumulates the session, recompiles via the full Typhon pipeline,
//! and executes the result with a Python interpreter. Only the *new* tail
//! of stdout is shown after each input, so earlier `print(...)` calls are
//! not re-displayed every iteration.
//!
//! Limitations:
//!   - **Re-execution semantics.** Each prompt re-runs the entire
//!     accumulated session against a fresh interpreter. Any side effects
//!     in prior blocks (network calls, file writes, mutation of external
//!     state, `random`, timestamps) therefore fire *once per prompt*.
//!     Treat the REPL as a scratch pad for pure code; reach for `tyc
//!     build` + `python` for anything with externally visible effects.
//!   - **"New output" diffing.** The visible tail is computed as the
//!     suffix of the new stdout past the previous run's stdout length.
//!     When earlier prints are non-deterministic the diff is best-effort
//!     and can either show "old" lines again or hide changes. The
//!     stdout slice is taken on a char boundary, so a divergent prefix
//!     containing multi-byte characters never panics.
//!   - **Multi-line blocks** end on the first blank line — matching
//!     Python's own REPL, which is also unable to contain a blank line
//!     inside a `def` / `class` body when typed directly.
//!   - **Auto-print** of bare single-line expression statements: a
//!     prompt like `>>> 1 + 1` is rewritten to `print(repr(1 + 1))`
//!     before compiling so the user sees `2` immediately. Multi-line
//!     blocks, assignments, and any statement starting with a keyword
//!     (`let`, `def`, `if`, ...) are left untouched.
//!   - No readline support; arrow keys insert escape sequences.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use clap::Args;
use miette::{miette, Result};

use tyc_db::{check_file, TycDatabase};
use tyc_desugar::{desugar_module_with, DesugarOptions};
use tyc_emit::emit_python_with_line_offsets;
use tyc_syntax::preprocess::{
    expand_gather_blocks, expand_go_calls, expand_lazy_imports, expand_multiline_guards,
    expand_pipes, expand_question_ops, expand_with_chains, preprocess,
};

/// Arguments for `tyc repl`.
#[derive(Args, Debug)]
pub struct ReplArgs {
    /// Python interpreter to use.  When omitted, `tyc` searches for
    /// `python3.13`, `python3.12`, then `python3` on `PATH`.
    #[arg(long, value_name = "PATH")]
    pub python: Option<String>,

    /// Pre-load this `.ty` file as the initial session before prompting.
    #[arg(long, value_name = "FILE")]
    pub load: Option<PathBuf>,
}

pub fn run(args: ReplArgs) -> Result<()> {
    let python = match args.python.clone() {
        Some(p) => p,
        None => discover_python().ok_or_else(|| {
            miette!(
                "no Python interpreter found on PATH (tried python3.13, python3.12, python3); \
                 pass --python <path>"
            )
        })?,
    };
    let mut session = ReplSession::new(python.clone());

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
        python
    );

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut stdin = stdin.lock();
    let mut stdout = stdout.lock();

    let mut buf = String::new();
    // Holds a line that closed a previous multi-line block as its
    // dedent-to-0 terminator. When set, it is processed as the next
    // top-level prompt instead of reading stdin.
    let mut pending: Option<String> = None;
    loop {
        let line_owned: String = if let Some(p) = pending.take() {
            p
        } else {
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
            buf.trim_end_matches(['\r', '\n']).to_owned()
        };
        let line = line_owned.as_str();
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

        // Multi-line: if the line ends with `:` or `\`, keep reading until
        // a block terminator.  Termination rules depend on what opened
        // the continuation:
        //   • A `:`-opened block (def/class/if/...) terminates on the
        //     first blank line OR a non-blank line that returns to
        //     column 0.  The dedent line is carried over to the next
        //     prompt as a fresh top-level statement so a sibling
        //     declaration typed right after the body isn't lost.
        //   • A `\`-opened block (explicit line continuation) only
        //     terminates on a blank line: a continuation by definition
        //     starts at any column, and the user has no other way to
        //     end the expression.
        // EOF mid-block exits cleanly rather than half-compiling a torn block.
        let mut block = String::from(line);
        block.push('\n');
        let mut hit_eof = false;
        let mut carryover: Option<String> = None;
        let colon_opened = line.trim_end().ends_with(':');
        if needs_continuation(line) {
            loop {
                write!(stdout, "... ").ok();
                stdout.flush().ok();
                buf.clear();
                let n = stdin
                    .read_line(&mut buf)
                    .map_err(|e| miette!("read failed: {e}"))?;
                if n == 0 {
                    hit_eof = true;
                    break;
                }
                if buf.trim().is_empty() {
                    break;
                }
                let body = buf.trim_end_matches(['\r', '\n']);
                if colon_opened {
                    let indented = body.starts_with([' ', '\t']);
                    if !indented {
                        carryover = Some(body.to_owned());
                        break;
                    }
                }
                block.push_str(body);
                block.push('\n');
            }
        }
        if hit_eof {
            writeln!(stdout).ok();
            break;
        }
        if let Some(next) = carryover {
            pending = Some(next);
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

/// Holds the accumulated `.ty` source plus the cached emit and last-run
/// stdout, so each prompt does one compile (in `feed_block`) and one
/// subprocess invocation (in `evaluate`).
struct ReplSession {
    python: String,
    blocks: Vec<String>,
    /// Emitted Python from the most-recently-accepted `feed_block`,
    /// reused by `evaluate`.
    cached_py: Option<String>,
    /// Previous run's stdout, for computing the new tail without
    /// slicing on a byte offset that might split a UTF-8 code point.
    last_stdout: String,
}

impl ReplSession {
    fn new(python: String) -> Self {
        Self {
            python,
            blocks: Vec::new(),
            cached_py: None,
            last_stdout: String::new(),
        }
    }

    fn reset(&mut self) {
        self.blocks.clear();
        self.cached_py = None;
        self.last_stdout.clear();
    }

    fn source(&self) -> String {
        self.blocks.concat()
    }

    /// Type-check `block` in the *context* of the current session before
    /// committing it.  On success the emitted Python is cached so the
    /// follow-up `evaluate` call doesn't have to re-compile.
    fn feed_block(&mut self, block: &str) -> Result<()> {
        // FINDINGS #25: a bare expression block (`1 + 1`, `x + 1`) should
        // print its value — that's the universal REPL UX expectation. If
        // the block looks like a single expression statement, wrap it in
        // `print(repr(...))` before compiling.
        let effective_block = wrap_bare_expression_for_repl(block);
        let mut trial = self.source();
        if !trial.ends_with('\n') {
            trial.push('\n');
        }
        trial.push_str(&effective_block);
        let py = compile_to_python(&trial)?;
        self.blocks.push(if effective_block.ends_with('\n') {
            effective_block.clone()
        } else {
            format!("{effective_block}\n")
        });
        self.cached_py = Some(py);
        Ok(())
    }

    /// Run the cached compile under the configured Python and return the
    /// *new* stdout tail since the previous run.  Returns `None` when the
    /// session is empty.
    ///
    /// The "new tail" is computed by character-prefix comparison rather
    /// than byte slicing, so a divergent prefix containing multi-byte
    /// characters never lands inside a code point.
    fn evaluate(&mut self) -> Result<Option<String>> {
        if self.cached_py.is_none() {
            // Empty session, or a prior failed feed_block — nothing to run.
            return Ok(None);
        }
        let py = self.cached_py.clone().unwrap();

        let tmp = tempfile::Builder::new()
            .prefix("tyc-repl-")
            .suffix(".py")
            .tempfile()
            .map_err(|e| miette!("cannot create temp file: {e}"))?;
        std::fs::write(tmp.path(), &py).map_err(|e| miette!("cannot write temp file: {e}"))?;

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
        let new_tail = char_suffix_after(&self.last_stdout, &stdout);
        self.last_stdout = stdout;
        Ok(Some(new_tail))
    }
}

/// Return the suffix of `new` that follows the longest *character*-aligned
/// common prefix shared with `old`. Never slices inside a UTF-8 code point.
///
/// When `old` is a prefix of `new` (the common stable case for a growing
/// session), this is exactly the new tail. When the runs diverge, it
/// surfaces from the first differing character on — a best-effort signal
/// that says "something changed", which matches what an interactive user
/// expects more than a panic does.
fn char_suffix_after(old: &str, new: &str) -> String {
    let mut common_bytes = 0;
    for (a, b) in old.chars().zip(new.chars()) {
        if a != b {
            break;
        }
        common_bytes += a.len_utf8();
    }
    new[common_bytes..].to_owned()
}

/// Probe `PATH` for the best available Python interpreter (3.13 first,
/// falling back to 3.12, then plain `python3`). Returns `None` when none
/// of them spawn successfully — callers should surface that as a clear
/// error rather than letting the eventual subprocess fail.
fn discover_python() -> Option<String> {
    for candidate in ["python3.13", "python3.12", "python3"] {
        if Command::new(candidate)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Some(candidate.to_owned());
        }
    }
    None
}

// ── compile pipeline ────────────────────────────────────────────────────────

/// Run the full preprocessing + check + desugar + emit pipeline on `source`
/// and return the emitted Python text. Diagnostics are surfaced as miette
/// errors with a one-line summary; the full set is dropped (the REPL is for
/// interactive use, not CI).
/// If `block` is a single bare expression statement (`1 + 1`, `x + 1`,
/// `f(x)`, etc.), return a wrapped version that prints its repr. The
/// detection is text-based and intentionally conservative: anything
/// that looks like a statement (starts with a known keyword or
/// contains a top-level `=`) is returned unchanged.
fn wrap_bare_expression_for_repl(block: &str) -> String {
    // Only single-line, single-statement blocks are eligible.
    let stripped = block.trim_end_matches(['\n', '\r']);
    if stripped.contains('\n') {
        return block.to_owned();
    }
    let trimmed = stripped.trim();
    if trimmed.is_empty() {
        return block.to_owned();
    }
    // Reject anything that's already a statement / declaration / control flow.
    const STATEMENT_PREFIXES: &[&str] = &[
        "let ",
        "mut ",
        "def ",
        "class ",
        "if ",
        "elif ",
        "else:",
        "else ",
        "while ",
        "for ",
        "import ",
        "from ",
        "return",
        "raise",
        "try:",
        "try ",
        "except",
        "finally",
        "with ",
        "match ",
        "case ",
        "pass",
        "break",
        "continue",
        "global ",
        "nonlocal ",
        "async ",
        "await ",
        "go ",
        "gather:",
        "gather ",
        "yield",
        "@",
        "#",
        "lazy ",
        "comptime ",
        "interface ",
        "extend ",
        "impl ",
        "impl[",
        "model ",
        "unsafe",
        "class!",
    ];
    if STATEMENT_PREFIXES
        .iter()
        .any(|p| trimmed.starts_with(p) || trimmed == p.trim_end_matches([' ', ':']))
    {
        return block.to_owned();
    }
    // Reject lines containing a top-level `=` (assignment / augmented
    // assignment) — those produce no value to print. Walk depth so
    // `f(a=1)` doesn't trip the check; reject `==`, `>=`, `<=`, `!=`
    // by requiring the `=` not be part of a comparison op.
    let bytes = trimmed.as_bytes();
    let mut depth = 0i32;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'=' if depth == 0 => {
                let prev = i.checked_sub(1).map(|j| bytes[j]).unwrap_or(0);
                let next = bytes.get(i + 1).copied().unwrap_or(0);
                let is_comparison = matches!(prev, b'=' | b'!' | b'<' | b'>') || next == b'=';
                if !is_comparison {
                    return block.to_owned();
                }
            }
            _ => {}
        }
        i += 1;
    }
    // Skip the auto-print wrap when the bare expression is itself a
    // call to a known None-returning builtin (FINDINGS #53). Otherwise
    // `print(x)` becomes `print(repr(print(x)))` — the inner print
    // emits `x`'s repr, then the outer prints `None`. Detection is
    // syntactic: matches `print(...)` and the rarer `pprint.pprint(...)`.
    if is_none_returning_top_level_call(trimmed) {
        return block.to_owned();
    }
    format!("print(repr({}))\n", trimmed)
}

/// Return `true` when `expr` is a top-level call to a function that's
/// known to return `None` and produce visible side-effects on its own
/// (so re-wrapping in `print(repr(...))` would double-render). Used
/// by the REPL auto-print pass (FINDINGS #53).
fn is_none_returning_top_level_call(expr: &str) -> bool {
    const NONE_CALLS: &[&str] = &["print(", "pprint(", "pprint.pprint("];
    for prefix in NONE_CALLS {
        if expr.starts_with(prefix) && expr.ends_with(')') {
            return true;
        }
    }
    false
}

pub(crate) fn compile_to_python(source: &str) -> Result<String> {
    let expanded = expand_question_ops(&expand_pipes(&expand_with_chains(&expand_go_calls(
        &expand_gather_blocks(&expand_multiline_guards(&expand_lazy_imports(source))),
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
    let (py, _offsets) = emit_python_with_line_offsets(&desugar.module);
    Ok(py)
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
        assert!(
            py.contains("42"),
            "emitted python should mention `42`: {py}"
        );
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
        s.feed_block("let x: int = 1\n")
            .expect("good block accepted");
        let err = s.feed_block("let y: int = \"nope\"\n");
        assert!(err.is_err(), "bad block must be rejected");
        // The good block is still in the session.
        assert!(s.source().contains("x: int = 1"));
        assert!(
            !s.source().contains("nope"),
            "bad block must not be committed"
        );
    }

    #[test]
    fn session_reset_clears_state() {
        let mut s = ReplSession::new("python3".into());
        s.feed_block("let x: int = 1\n").unwrap();
        s.last_stdout = "previous output".into();
        s.reset();
        assert!(s.source().is_empty());
        assert!(s.last_stdout.is_empty());
        assert!(s.cached_py.is_none());
    }

    #[test]
    fn feed_block_caches_emitted_python_for_evaluate() {
        let mut s = ReplSession::new("python3".into());
        assert!(s.cached_py.is_none());
        s.feed_block("let x: int = 1\n").unwrap();
        let cached = s.cached_py.as_ref().expect("cache should be populated");
        // Cached output should be the same compile_to_python would produce.
        let direct = compile_to_python("let x: int = 1\n").unwrap();
        assert_eq!(cached, &direct);
    }

    // ── char_suffix_after ───────────────────────────────────────────────────

    #[test]
    fn char_suffix_after_returns_pure_tail_when_old_is_prefix() {
        assert_eq!(char_suffix_after("hello\n", "hello\nworld\n"), "world\n");
    }

    #[test]
    fn char_suffix_after_returns_empty_when_outputs_match() {
        assert_eq!(char_suffix_after("abc", "abc"), "");
    }

    #[test]
    fn char_suffix_after_handles_multibyte_safely() {
        // Common prefix is "héllo " (7 bytes), then the new run diverges.
        // A naive `new[old.len()..]` would slice inside the é.
        let old = "héllo ";
        let new = "héllo world";
        assert_eq!(char_suffix_after(old, new), "world");
    }

    #[test]
    fn char_suffix_after_surfaces_divergent_tail_from_divergence_point() {
        // First two chars match, third differs — surface from the diverging
        // character on, never panic.
        let old = "abc";
        let new = "abXYZ";
        assert_eq!(char_suffix_after(old, new), "XYZ");
    }

    #[test]
    fn char_suffix_after_empty_old_returns_full_new() {
        assert_eq!(char_suffix_after("", "anything"), "anything");
    }

    // ── discover_python ─────────────────────────────────────────────────────

    #[test]
    fn discover_python_returns_some_when_interpreter_present() {
        // CI always provides at least one Python; skip when absent.
        if let Some(name) = discover_python() {
            assert!(
                ["python3.13", "python3.12", "python3"].contains(&name.as_str()),
                "unexpected interpreter: {name}"
            );
        }
    }
}
