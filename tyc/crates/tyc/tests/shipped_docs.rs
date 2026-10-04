//! The documentation that ships *inside* the binary must be valid Typhon.
//!
//! `docs/cheatsheet.md` is `include_str!`'d into `tyc cheatsheet`, so every
//! snippet in it is something the tool actively teaches. Three of them did not
//! parse: the `## Concurrency` block spelled `gather:` bindings with `let` and
//! `go` (the grammar accepts neither), four pages showed a prefix
//! `frozen class X:` (the modifier is postfix), and `lazy import pandas`
//! silently emitted an *eager* import.
//!
//! Nothing checked any of it, which is why it drifted. This test extracts the
//! Typhon snippets and runs them through the real front end.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `<repo>/tyc/crates/tyc`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repo root")
        .to_path_buf()
}

fn tyc() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tyc"))
}

/// Snippets in `cheatsheet.md` are 4-space-indented blocks, not fenced. Pull
/// out each contiguous run of indented lines.
fn indented_blocks(markdown: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Vec<String> = Vec::new();
    for line in markdown.lines() {
        if let Some(rest) = line.strip_prefix("    ") {
            current.push(rest.to_owned());
        } else if line.trim().is_empty() {
            // A blank line inside a block keeps it open.
            if !current.is_empty() {
                current.push(String::new());
            }
        } else {
            if !current.is_empty() {
                blocks.push(current.join("\n"));
                current.clear();
            }
        }
    }
    if !current.is_empty() {
        blocks.push(current.join("\n"));
    }
    blocks
}

/// A block is Typhon source we can check when it is not a shell/CLI listing
/// and not a prose table.
fn is_typhon_snippet(block: &str) -> bool {
    let code: Vec<&str> = block
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    if code.is_empty() {
        return false;
    }
    // `tyc build   # …` command listings, and `$ …` shell lines.
    if code
        .iter()
        .all(|l| l.trim_start().starts_with("tyc ") || l.trim_start().starts_with('$'))
    {
        return false;
    }
    // `[project]` / `key = "value"` TOML listings.
    if code
        .iter()
        .any(|l| l.trim_start().starts_with('[') && l.trim_end().ends_with(']'))
    {
        return false;
    }
    true
}

/// Wrap a snippet so a bare statement sequence is a checkable module.
///
/// Snippets are written as body fragments (`let x: int = 1`), which are only
/// legal inside a function. Indenting them into one gives the front end
/// something whole to parse without changing what is being tested. Snippets
/// that already declare top-level constructs are used as-is.
fn as_module(snippet: &str) -> String {
    let declares_top_level = snippet.lines().any(|l| {
        let t = l.trim_start();
        l.starts_with(|c: char| !c.is_whitespace())
            && (t.starts_with("def ")
                || t.starts_with("async def ")
                || t.starts_with("class ")
                || t.starts_with("plain class ")
                || t.starts_with("model ")
                || t.starts_with("interface ")
                || t.starts_with("impl ")
                || t.starts_with("extend ")
                || t.starts_with("enum ")
                || t.starts_with("type ")
                || t.starts_with("newtype ")
                || t.starts_with("pub ")
                || t.starts_with("import ")
                || t.starts_with("from ")
                || t.starts_with("lazy import ")
                || t.starts_with("freeze let ")
                || t.starts_with("comptime "))
    });
    if declares_top_level {
        return snippet.to_owned();
    }
    // `gather:` / `go` / `await` are only legal inside an `async def`.
    let needs_async = snippet.contains("gather:")
        || snippet.contains("await ")
        || snippet.lines().any(|l| l.trim_start().starts_with("go "));
    let header = if needs_async {
        "async def __snippet__() -> None:"
    } else {
        "def __snippet__() -> None:"
    };
    let body: String = snippet
        .lines()
        .map(|l| {
            if l.trim().is_empty() {
                String::new()
            } else {
                format!("    {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{header}\n{body}\n")
}

#[test]
fn every_cheatsheet_snippet_parses() {
    let root = repo_root();
    let markdown = std::fs::read_to_string(root.join("docs/cheatsheet.md"))
        .expect("docs/cheatsheet.md is include_str!'d by `tyc cheatsheet`");
    let tmp = tempfile::tempdir().unwrap();

    let mut failures: Vec<String> = Vec::new();
    for (i, block) in indented_blocks(&markdown).into_iter().enumerate() {
        if !is_typhon_snippet(&block) {
            continue;
        }
        // Snippets deliberately showing a rejected form mark themselves.
        if block.contains("does not parse") || block.contains("❌") {
            continue;
        }
        let dir = tmp.path().join(format!("s{i}"));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("typhon.toml"),
            "[project]\nname = \"snippet\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
             [python]\ntarget = \"3.13\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("src").join("main.ty"), as_module(&block)).unwrap();

        let out = tyc()
            .arg("check")
            .arg(dir.join("src"))
            .output()
            .expect("tyc check runs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        // Only *parse* failures are asserted on. A snippet legitimately
        // references names it does not define (`fetch`, `url1`, `User`), so
        // unknown-name and type errors are expected and not the point here —
        // the point is that `tyc cheatsheet` never teaches syntax the grammar
        // rejects.
        if text.contains("tyc::parse") {
            failures.push(format!("--- snippet {i} ---\n{block}\n--- {text}"));
        }
    }

    assert!(
        failures.is_empty(),
        "cheatsheet snippets that do not parse:\n\n{}",
        failures.join("\n")
    );
}

#[test]
fn docs_do_not_teach_the_prefix_frozen_modifier() {
    // The modifier is postfix: `class X frozen:`. The prefix spelling is a
    // hard parse error, and it was taught on four pages — including the one
    // `tyc explain frozen_assign` prints, where it was the only example.
    let root = repo_root();
    let mut offenders = Vec::new();
    for rel in [
        "docs",
        ".claude/skills/typhon",
        // The copy of the skill embedded in the crate — it ships with the
        // binary, and the pre-PR review found the prefix spelling fixed in
        // the `.claude` copy but still taught here.
        "tyc/crates/tyc/skill",
        "docs-site/src/content/docs",
    ] {
        let dir = root.join(rel);
        if !dir.is_dir() {
            continue;
        }
        let mut stack = vec![dir];
        while let Some(d) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if !matches!(
                    path.extension().and_then(|e| e.to_str()),
                    Some("md" | "mdx")
                ) {
                    continue;
                }
                // The review documents quote the broken form as evidence.
                if path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("codebase-review-"))
                {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for (n, line) in text.lines().enumerate() {
                    let t = line.trim_start();
                    if t.starts_with("frozen class ") || t.contains("pub frozen class") {
                        offenders.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                    }
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "prefix `frozen class` is not valid Typhon; use `class X frozen:`:\n{}",
        offenders.join("\n")
    );
}

/// Every `tyc::<code>` named in the live documentation must be a code the
/// binary actually emits. Doc pages kept citing `tyc::comptime_env_missing`,
/// `tyc::lazy_from_import` and `tyc::default_mismatch` long after those codes
/// never existed — each renders as a confident, unresolvable link. This test
/// extracts every code token from the live doc surfaces and checks it against
/// `tyc explain --list`, so a rename or a typo fails loudly instead of
/// shipping a dead reference.
#[test]
fn docs_only_name_emitted_diagnostic_codes() {
    let root = repo_root();

    // The binary is the source of truth, not a checked-in list.
    let out = tyc()
        .arg("explain")
        .arg("--list")
        .output()
        .expect("tyc explain --list runs");
    assert!(out.status.success(), "`tyc explain --list` must succeed");
    let listing = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let emitted: std::collections::HashSet<String> = code_tokens(&listing).into_iter().collect();
    assert!(
        !emitted.is_empty(),
        "`tyc explain --list` produced no codes to check against"
    );

    // Frozen historical records: dated reviews, audits, design proposals and
    // the roadmap name codes that were proposed, renamed or never built.
    // They are history and must not be rewritten to match the present.
    let historical: &[&str] = &[
        "docs/reviews/adversarial-audit-50agent-2026-06-28.md",
        "docs/reviews/adversarial-review-2026-06-28.md",
        "docs/reviews/codebase-review-2026-07-28.md",
        "docs/reviews/codebase-review-2026-07-28-findings.md",
        "docs/reviews/beta-readiness-review-2026-09-01.md",
        "docs/reviews/project-review-2026-05-16.md",
        "docs/reviews/release-readiness-2026-07-20.md",
        "docs/reviews/release-readiness-review-2026-09-30.md",
        "docs/reviews/RELEASE_READINESS_REVIEW.md",
        "docs/alpha-release-plan.md",
        "docs/roadmap.md",
    ];
    let historical_dirs: &[&str] = &["docs/design"];

    // Tokens that are deliberately not emitted codes: worked-example
    // placeholders used by the authoring guides (`internals/adding-diagnostic`
    // walks through a fictional `tyc::my_new_check`; `reading` and
    // `internals/workspace` use `tyc::some_code` / `tyc::CODE_NAME` the same
    // way). Anything else must resolve in `tyc explain --list` — including
    // the `freeze` / `pub` family names, which are pages, not codes.
    let allowed: &[&str] = &[
        "some_code",
        "code",
        "code_name",
        "CODE",
        "CODE_NAME",
        "my_new_check",
    ];

    let mut offenders = Vec::new();
    for rel in [
        "docs",
        "docs-site/src/content/docs",
        ".claude/skills/typhon",
        // The copy of the skill embedded in the crate ships with the binary.
        "tyc/crates/tyc/skill",
    ] {
        let dir = root.join(rel);
        if !dir.is_dir() {
            continue;
        }
        let mut stack = vec![dir];
        while let Some(d) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if !matches!(
                    path.extension().and_then(|e| e.to_str()),
                    Some("md" | "mdx")
                ) {
                    continue;
                }
                let rel_path = path
                    .strip_prefix(&root)
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                if historical.iter().any(|h| rel_path == *h)
                    || historical_dirs
                        .iter()
                        .any(|h| rel_path == *h || rel_path.starts_with(&format!("{h}/")))
                {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for (n, line) in text.lines().enumerate() {
                    // `## tyc::foo (reserved)` headers name a code on
                    // purpose: the name is held for a future rule and the
                    // section says it is not emitted yet.
                    if line.contains("reserved") {
                        continue;
                    }
                    for token in code_tokens(line) {
                        // Family-prefix prose (`tyc::perf_*`) leaves a
                        // trailing-underscore fragment; no real code ends
                        // in `_`.
                        if token.ends_with('_') {
                            continue;
                        }
                        if allowed.contains(&token.as_str()) {
                            continue;
                        }
                        if !emitted.contains(&token) {
                            offenders.push(format!("{}:{}: tyc::{}", path.display(), n + 1, token));
                        }
                    }
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "docs name `tyc::` codes the binary does not emit:\n{}",
        offenders.join("\n")
    );
}

/// Every diagnostic code the compiler can construct is documented on all
/// three surfaces a reader reaches it from: `tyc explain --list`, its
/// `docs/diagnostics/<code>.md` page (embedded for `tyc explain <code>`), and
/// a section of the docs site's diagnostics catalog that the catalog index
/// (`diagnostics/reading.mdx`) names. The docs site covered 48 of 92 codes
/// before this test existed — the gap only shows up when someone searches
/// the published site for a code they were just shown.
#[test]
fn every_diagnostic_code_is_documented_everywhere() {
    let root = repo_root();

    let out = tyc()
        .arg("explain")
        .arg("--list")
        .output()
        .expect("tyc explain --list runs");
    assert!(out.status.success(), "`tyc explain --list` must succeed");
    let listed: std::collections::BTreeSet<String> =
        code_tokens(&String::from_utf8_lossy(&out.stdout))
            .into_iter()
            .collect();
    assert!(!listed.is_empty(), "`tyc explain --list` listed no codes");

    // The codes the compiler can construct: every `code(tyc::NAME)` in a
    // `#[diagnostic(...)]` attribute anywhere in the workspace sources.
    let mut declared = std::collections::BTreeSet::new();
    let mut stack = vec![root.join("tyc/crates")];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Integration tests (this file among them) mention the
                // attribute shape in prose; only compiler sources declare.
                if path.file_name().and_then(|n| n.to_str()) != Some("tests") {
                    stack.push(path);
                }
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (i, _) in text.match_indices("code(tyc::") {
                let rest = &text[i + "code(".len()..];
                if let Some(token) = code_tokens(rest).into_iter().next() {
                    // Real codes are snake_case; this skips placeholders
                    // such as `code(tyc::NAME)` in doc comments.
                    if token
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                    {
                        declared.insert(token);
                    }
                }
            }
        }
    }
    assert!(
        !declared.is_empty(),
        "found no `code(tyc::…)` declarations under tyc/crates"
    );

    let site_dir = root.join("docs-site/src/content/docs/diagnostics");
    let mut site_headings = std::collections::BTreeSet::new();
    if let Ok(entries) = std::fs::read_dir(&site_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("mdx") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            for line in text.lines().filter(|l| l.starts_with('#')) {
                site_headings.extend(code_tokens(line));
            }
        }
    }
    let index = std::fs::read_to_string(site_dir.join("reading.mdx"))
        .expect("docs-site diagnostics index (reading.mdx) exists");

    let mut problems = Vec::new();
    for code in declared.difference(&listed) {
        problems.push(format!(
            "tyc::{code} is declared in the compiler but missing from `tyc explain --list`"
        ));
    }
    for code in &listed {
        if !root.join(format!("docs/diagnostics/{code}.md")).is_file() {
            problems.push(format!("tyc::{code} has no docs/diagnostics/{code}.md"));
        }
        if !site_headings.contains(code) {
            problems.push(format!(
                "tyc::{code} has no `## `tyc::{code}`` section under docs-site/src/content/docs/diagnostics/"
            ));
        }
        if !index.contains(&format!("`{code}`")) {
            problems.push(format!(
                "tyc::{code} is not listed in the docs-site diagnostics index (diagnostics/reading.mdx)"
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "diagnostic documentation gaps:\n{}",
        problems.join("\n")
    );
}

/// Pull `tyc::<code>` tokens out of text. A token is the maximal run of
/// ASCII alphanumerics and underscores after the prefix; an empty run (a
/// bare `tyc::` in prose) yields nothing.
fn code_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 5 <= bytes.len() {
        if &bytes[i..i + 5] == b"tyc::" {
            let mut j = i + 5;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            if j > i + 5 {
                if let Ok(token) = std::str::from_utf8(&bytes[i + 5..j]) {
                    out.push(token.to_owned());
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}
