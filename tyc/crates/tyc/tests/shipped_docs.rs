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
