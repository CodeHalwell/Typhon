//! The reserved `typhon_runtime` module name (W4-12, `tyc::reserved_module_name`).
//!
//! `tyc build` writes a generated `typhon_runtime/` package next to the
//! emitted Python whenever the program uses `Result`, `go`, `lazy`, … A
//! project module of the same name — `src/typhon_runtime.ty` or a
//! `src/typhon_runtime/` package — is then shadowed (a package beats a module
//! of the same name) or overwritten file by file. Check and build were both
//! clean and the program failed at import time:
//! `ImportError: cannot import name 'helper' from 'typhon_runtime'`.
//!
//! The name is reserved. `tyc check` warns whenever a project defines it.
//! `tyc build` refuses only when the program needs the generated runtime *and*
//! imports something from `typhon_runtime` that the generated runtime will not
//! provide — a program that already fails on import. Every other case keeps
//! building (it works today) with a warning to rename the module.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use miette::{miette, Severity};

/// The diagnostic's catalog page.
const URL: &str =
    "https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/reserved_module_name.md";

/// A project's own `typhon_runtime`, found directly under the source root.
#[derive(Debug)]
pub(crate) struct UserRuntime {
    /// The module file or package directory.
    pub path: PathBuf,
    /// For a package, the stems of its modules (`helpers` for
    /// `typhon_runtime/helpers.ty`), excluding `__init__`.
    pub submodules: Vec<String>,
}

/// The project's `typhon_runtime` module or package under `src_dir`, if any.
pub(crate) fn user_runtime(src_dir: &Path) -> Option<UserRuntime> {
    let package = src_dir.join("typhon_runtime");
    if package.is_dir() {
        let mut submodules: Vec<String> = std::fs::read_dir(&package)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                let is_source = p
                    .extension()
                    .is_some_and(|x| x == "ty" || x == "dty" || x == "py");
                let stem = p.file_stem()?.to_str()?.to_owned();
                if p.is_dir() || (is_source && stem != "__init__") {
                    Some(stem)
                } else {
                    None
                }
            })
            .collect();
        submodules.sort();
        submodules.dedup();
        return Some(UserRuntime {
            path: package,
            submodules,
        });
    }
    ["ty", "dty", "py"]
        .iter()
        .map(|ext| src_dir.join(format!("typhon_runtime.{ext}")))
        .find(|p| p.is_file())
        .map(|path| UserRuntime {
            path,
            submodules: Vec::new(),
        })
}

/// Names a generated Python module binds at top level (what `from it import
/// NAME` can import): `class`/`def` names, assignment targets, and names bound
/// by imports. A line scan is enough for the templates `tyc` itself writes.
fn bound_names(python: &str) -> HashSet<String> {
    let ident = |s: &str| -> Option<String> {
        let name: String = s
            .trim_start()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        (!name.is_empty()).then_some(name)
    };
    let mut names = HashSet::new();
    for line in python.lines() {
        if line.starts_with([' ', '\t', '#']) {
            continue;
        }
        if let Some(rest) = line
            .strip_prefix("class ")
            .or_else(|| line.strip_prefix("def "))
            .or_else(|| line.strip_prefix("async def "))
        {
            names.extend(ident(rest));
        } else if let Some(rest) = line.strip_prefix("from ") {
            if let Some((_, imported)) = rest.split_once(" import ") {
                for item in imported.split('#').next().unwrap_or("").split(',') {
                    let item = item.trim().trim_matches(['(', ')']);
                    let bound = item.rsplit(" as ").next().unwrap_or(item);
                    names.extend(ident(bound));
                }
            }
        } else if let Some(rest) = line.strip_prefix("import ") {
            for item in rest.split(',') {
                let item = item.trim();
                let bound = match item.split_once(" as ") {
                    Some((_, alias)) => alias,
                    None => item.split('.').next().unwrap_or(item),
                };
                names.extend(ident(bound));
            }
        } else if let Some(name) = ident(line) {
            let after = line.trim_start()[name.len()..].trim_start();
            if (after.starts_with('=') && !after.starts_with("==")) || after.starts_with(':') {
                names.insert(name);
            }
        }
    }
    names
}

/// One `typhon_runtime` import in project source that will fail at import
/// time once the generated runtime replaces the project's module.
#[derive(Debug)]
pub(crate) struct BrokenImport {
    pub file: PathBuf,
    pub line: usize,
    pub statement: String,
    pub missing: String,
}

/// Logical import statements of `source` that name `typhon_runtime`, with
/// their 1-based line (parenthesised continuations joined).
fn runtime_imports(source: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut lines = source.lines().enumerate();
    while let Some((i, line)) = lines.next() {
        let trimmed = line.trim_start();
        let is_import = trimmed.starts_with("from typhon_runtime")
            || trimmed.starts_with("import typhon_runtime");
        if !is_import {
            continue;
        }
        let mut statement = trimmed.split('#').next().unwrap_or("").trim().to_owned();
        if statement.contains('(') && !statement.contains(')') {
            for (_, more) in lines.by_ref() {
                let more = more.split('#').next().unwrap_or("").trim();
                statement.push(' ');
                statement.push_str(more);
                if more.contains(')') {
                    break;
                }
            }
        }
        out.push((i + 1, statement));
    }
    out
}

/// The imports in `sources` that fail once the generated runtime (`files`:
/// name → contents) replaces `user`.
pub(crate) fn broken_imports(
    sources: &[(PathBuf, String)],
    user: &UserRuntime,
    files: &[(&str, String)],
) -> Vec<BrokenImport> {
    let generated_modules: HashSet<String> = files
        .iter()
        .filter_map(|(name, _)| name.strip_suffix(".py"))
        .filter(|stem| *stem != "__init__")
        .map(str::to_owned)
        .collect();
    let module_names = |stem: &str| -> HashSet<String> {
        files
            .iter()
            .find(|(name, _)| name.strip_suffix(".py") == Some(stem))
            .map(|(_, body)| bound_names(body))
            .unwrap_or_default()
    };
    // User submodules whose files survive: the runtime only writes its own
    // file names.
    let surviving: HashSet<String> = user
        .submodules
        .iter()
        .filter(|s| !generated_modules.contains(*s))
        .cloned()
        .collect();
    let mut package_names = module_names("__init__");
    package_names.extend(generated_modules.iter().cloned());
    package_names.extend(surviving.iter().cloned());

    let mut broken = Vec::new();
    for (file, source) in sources {
        for (line, statement) in runtime_imports(source) {
            let mut fail = |missing: String| {
                broken.push(BrokenImport {
                    file: file.clone(),
                    line,
                    statement: statement.clone(),
                    missing,
                })
            };
            if let Some(rest) = statement.strip_prefix("from ") {
                let Some((module, imported)) = rest.split_once(" import ") else {
                    continue;
                };
                let module = module.trim();
                let available = if module == "typhon_runtime" {
                    Some(package_names.clone())
                } else if let Some(sub) = module.strip_prefix("typhon_runtime.") {
                    let top = sub.split('.').next().unwrap_or(sub);
                    if surviving.contains(top) {
                        None // the project's own submodule: unaffected
                    } else if generated_modules.contains(top) && !sub.contains('.') {
                        Some(module_names(top))
                    } else {
                        fail(format!("module `{module}`"));
                        continue;
                    }
                } else {
                    continue; // `typhon_runtimex` and the like
                };
                let Some(available) = available else {
                    continue;
                };
                for item in imported.trim().trim_matches(['(', ')']).split(',') {
                    let name = item.split_whitespace().next().unwrap_or("");
                    if name.is_empty() || name == "*" {
                        continue;
                    }
                    if !available.contains(name) {
                        fail(format!("`{name}`"));
                    }
                }
            } else if let Some(rest) = statement.strip_prefix("import ") {
                for item in rest.split(',') {
                    let module = item.split_whitespace().next().unwrap_or("");
                    let Some(sub) = module.strip_prefix("typhon_runtime.") else {
                        continue;
                    };
                    let top = sub.split('.').next().unwrap_or(sub);
                    if !surviving.contains(top) && !generated_modules.contains(top) {
                        fail(format!("module `{module}`"));
                    }
                }
            }
        }
    }
    broken
}

/// The warning both commands print when a project defines the reserved name.
pub(crate) fn reserved_warning(user: &UserRuntime, replaced_now: bool) -> miette::Report {
    let when = if replaced_now {
        "this build writes the generated runtime over it"
    } else {
        "the generated runtime replaces it as soon as the program uses `Result`, `go`, `lazy` \
         or another runtime feature"
    };
    miette!(
        severity = Severity::Warning,
        code = "tyc::reserved_module_name",
        url = URL,
        help = "rename the module (and its imports) — for example to `runtime_helpers`",
        "`typhon_runtime` is reserved for the runtime `tyc build` generates: '{}' — {when}",
        user.path.display()
    )
}

/// The error `tyc build` returns when the replacement breaks the program.
pub(crate) fn reserved_error(user: &UserRuntime, broken: &[BrokenImport]) -> miette::Report {
    let list: Vec<String> = broken
        .iter()
        .map(|b| {
            format!(
                "  {}:{}: `{}` — {} is not provided by the generated runtime",
                b.file.display(),
                b.line,
                b.statement,
                b.missing
            )
        })
        .collect();
    miette!(
        code = "tyc::reserved_module_name",
        url = URL,
        help = "rename the module (and these imports) — for example to `runtime_helpers`",
        "`typhon_runtime` is reserved: this program uses the runtime `tyc build` generates, \
         which replaces '{}', so these imports would fail when the program starts:\n{}",
        user.path.display(),
        list.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bound_names_reads_top_level_bindings() {
        let names = bound_names(
            "from __future__ import annotations\nfrom . import lazy, tasks  # x\nimport os.path\n\
             _T = TypeVar(\"_T\")\nclass Ok:\n    value: int\ndef try_result(t):\n    x = 1\n\
             Result: TypeAlias = int\n__all__ = []\n",
        );
        for want in [
            "annotations",
            "lazy",
            "tasks",
            "os",
            "_T",
            "Ok",
            "try_result",
            "Result",
            "__all__",
        ] {
            assert!(names.contains(want), "{want} missing from {names:?}");
        }
        assert!(!names.contains("value") && !names.contains("x"));
    }

    #[test]
    fn imports_of_names_the_runtime_lacks_are_broken() {
        let files = vec![
            (
                "__init__.py",
                "from . import lazy\nclass Ok:\n    pass\n".to_owned(),
            ),
            ("lazy.py", "def lazy_import(m):\n    pass\n".to_owned()),
        ];
        let user = UserRuntime {
            path: PathBuf::from("src/typhon_runtime"),
            submodules: vec!["helpers".into(), "lazy".into()],
        };
        let src = "from typhon_runtime import Ok, helper\nfrom typhon_runtime.helpers import h\n\
                   from typhon_runtime.lazy import lazy_import, other\nimport typhon_runtime.nope\n\
                   from typhon_runtime import (\n    Ok,\n    helpers,\n)\n";
        let broken = broken_imports(&[(PathBuf::from("m.ty"), src.to_owned())], &user, &files);
        let missing: Vec<&str> = broken.iter().map(|b| b.missing.as_str()).collect();
        assert_eq!(
            missing,
            vec!["`helper`", "`other`", "module `typhon_runtime.nope`"]
        );
        assert_eq!(broken[0].line, 1);
    }
}
