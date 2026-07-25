//! Regression harness for the `examples/errors/` corpus.
//!
//! Every `.ty` file under `examples/errors/` is a *deliberately broken*
//! program. Each one declares, in a header comment, exactly which diagnostics
//! it should produce:
//!
//! ```text
//! # EXPECT-ERROR: tyc::missing_binding_kind
//! # EXPECT-WARN:  tyc::unused_import
//! # REQUIRES: build          (optional — run `tyc build` instead of `tyc check`)
//! ```
//!
//! Files under `examples/errors/12-known-gaps/` are the inverse: they carry a
//! `# KNOWN-GAP:` header and are asserted to produce **no** diagnostics at
//! all, documenting places where the checker is currently silent about a
//! program that fails at runtime. When one of those gaps is closed the
//! assertion fires, which is the signal to move the file into the matching
//! `NN-*/` directory with a real `# EXPECT-ERROR:` header.
//!
//! This is the piece that makes the corpus a regression net rather than a pile
//! of files: it runs under `cargo test --workspace`, so CI enforces it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<repo>/tyc/crates/tyc`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repo root above tyc/crates/tyc")
        .to_path_buf()
}

fn errors_dir() -> PathBuf {
    repo_root().join("examples").join("errors")
}

/// Every `.ty` file under `examples/errors/`, sorted for stable test output.
fn corpus() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![errors_dir()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "ty") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

#[derive(Default)]
struct Expectations {
    errors: BTreeSet<String>,
    warnings: BTreeSet<String>,
    requires_build: bool,
    known_gap: bool,
}

fn parse_expectations(source: &str) -> Expectations {
    let mut exp = Expectations::default();
    for line in source.lines() {
        let line = line.trim();
        if !line.starts_with('#') {
            // Directives live in the header block; stop at the first real code.
            if !line.is_empty() {
                break;
            }
            continue;
        }
        let body = line.trim_start_matches('#').trim();
        if let Some(code) = body.strip_prefix("EXPECT-ERROR:") {
            exp.errors.insert(normalise_code(code));
        } else if let Some(code) = body.strip_prefix("EXPECT-WARN:") {
            exp.warnings.insert(normalise_code(code));
        } else if body.starts_with("REQUIRES: build") {
            exp.requires_build = true;
        } else if body.starts_with("KNOWN-GAP:") {
            exp.known_gap = true;
        }
    }
    exp
}

fn normalise_code(raw: &str) -> String {
    raw.trim().trim_start_matches("tyc::").to_string()
}

/// Diagnostic codes actually produced, split into (errors, warnings).
///
/// The renderer prints one `tyc::<code> (https://…)` header per diagnostic,
/// followed by a body line whose leading glyph carries the severity: `×` for
/// an error, `⚠` for a warning, `☞` for advice (which the CLI counts as a
/// warning). Parsing the glyph rather than the `── errors in … ──` section
/// header is what lets the same parser handle `tyc build` output, which has no
/// section headers or summary block.
fn observed(output: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut errors = BTreeSet::new();
    let mut warnings = BTreeSet::new();
    let mut pending: Option<String> = None;

    for line in output.lines() {
        // The summary block repeats every code; stop before it so codes
        // aren't double-counted with the wrong severity.
        if line.contains("── summary ──") {
            break;
        }
        if let Some(code) = header_code(line) {
            pending = Some(code);
            continue;
        }
        let Some(code) = pending.clone() else {
            continue;
        };
        if line.trim().is_empty() {
            continue;
        }
        if line.contains('×') {
            errors.insert(code);
            pending = None;
        } else if line.contains('⚠') || line.contains('☞') {
            warnings.insert(code);
            pending = None;
        }
    }
    (errors, warnings)
}

/// Extract `foo` from a `tyc::foo (https://…)` diagnostic header line.
fn header_code(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("tyc::")?;
    if !rest.contains("(https") {
        return None;
    }
    let code: String = rest
        .chars()
        .take_while(|c| c.is_ascii_lowercase() || *c == '_')
        .collect();
    (!code.is_empty()).then_some(code)
}

/// Run `tyc check` on a single file, or `tyc build` on a scaffolded project
/// when the file declares `# REQUIRES: build`.
fn run_tyc(path: &Path, requires_build: bool) -> String {
    let out = if requires_build {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::copy(path, tmp.path().join("src").join("main.ty")).unwrap();
        std::fs::write(
            tmp.path().join("typhon.toml"),
            "[project]\nname = \"errors\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
             [python]\ntarget = \"3.13\"\n",
        )
        .unwrap();
        Command::new(env!("CARGO_BIN_EXE_tyc"))
            .args(["build", "--no-sync"])
            .current_dir(tmp.path())
            .output()
            .unwrap()
    } else {
        Command::new(env!("CARGO_BIN_EXE_tyc"))
            .arg("check")
            .arg(path)
            .output()
            .unwrap()
    };
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn error_examples_produce_exactly_their_declared_diagnostics() {
    let mut failures = Vec::new();
    let files = corpus();
    assert!(!files.is_empty(), "examples/errors/ corpus is empty");

    for path in &files {
        let source = std::fs::read_to_string(path).unwrap();
        let exp = parse_expectations(&source);
        let rel = path
            .strip_prefix(repo_root())
            .unwrap()
            .display()
            .to_string();

        if exp.known_gap {
            continue; // covered by the dedicated known-gap test below
        }
        if exp.errors.is_empty() && exp.warnings.is_empty() {
            failures.push(format!(
                "{rel}: no `# EXPECT-ERROR:` / `# EXPECT-WARN:` / `# KNOWN-GAP:` header. \
                 Every file in examples/errors/ must declare what it produces."
            ));
            continue;
        }

        let (errors, warnings) = observed(&run_tyc(path, exp.requires_build));
        if errors != exp.errors || warnings != exp.warnings {
            failures.push(format!(
                "{rel}:\n     expected errors:   {:?}\n     observed errors:   {:?}\n  \
                 expected warnings: {:?}\n     observed warnings: {:?}",
                exp.errors, errors, exp.warnings, warnings
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} error-example(s) did not match their declared diagnostics:\n\n{}",
        failures.len(),
        files.len(),
        failures.join("\n\n")
    );
}

/// Files under `12-known-gaps/` document programs the checker is currently
/// silent about. If one starts producing diagnostics, that is good news — but
/// the file needs to move into the matching `NN-*/` directory with a real
/// `# EXPECT-ERROR:` header, so fail loudly rather than drift.
#[test]
fn known_gaps_still_produce_no_diagnostics() {
    let mut closed = Vec::new();
    for path in corpus() {
        let source = std::fs::read_to_string(&path).unwrap();
        let exp = parse_expectations(&source);
        if !exp.known_gap {
            continue;
        }
        let (errors, warnings) = observed(&run_tyc(&path, exp.requires_build));
        if !errors.is_empty() || !warnings.is_empty() {
            closed.push(format!(
                "{}: now reports errors {:?} / warnings {:?}",
                path.strip_prefix(repo_root()).unwrap().display(),
                errors,
                warnings
            ));
        }
    }
    assert!(
        closed.is_empty(),
        "a known gap has been closed — move the file out of examples/errors/12-known-gaps/ \
         into the matching NN-*/ directory and give it an `# EXPECT-ERROR:` header:\n  {}",
        closed.join("\n  ")
    );
}

/// The corpus is only useful if it is discoverable, so keep the README's
/// per-directory table honest about which directories exist.
#[test]
fn every_error_directory_is_listed_in_the_readme() {
    let readme = std::fs::read_to_string(errors_dir().join("README.md")).unwrap();
    let mut missing = Vec::new();
    for entry in std::fs::read_dir(errors_dir()).unwrap() {
        let path = entry.unwrap().path();
        if !path.is_dir() {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !readme.contains(&name) {
            missing.push(name);
        }
    }
    missing.sort();
    assert!(
        missing.is_empty(),
        "examples/errors/README.md does not mention: {missing:?}"
    );
}
