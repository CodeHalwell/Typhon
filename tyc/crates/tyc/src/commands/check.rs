//! `tyc check` — parse, resolve, and type-check without emitting code.
//!
//! Runs the full Phase-1 pipeline: pre-process → parse → resolve
//! (scopes + `val`/`var` enforcement + unknown-name diagnostics) → type
//! check (nominal types, non-null narrowing, argument-count and
//! argument-type checks). Diagnostics are rendered via miette.

use std::path::PathBuf;

use clap::Args;
use miette::{miette, Result};

use tyc_analyse::{analyse_purity, evaluate_comptime_with_functions, purity_diagnostics};
use tyc_db::{check_file_with_imports, extract_shapes_for_path, TycDatabase};
use tyc_diagnostics::{Diagnostics, TycError};
use tyc_emit::{compare_modules, StubTestKind};
#[cfg(test)]
use tyc_resolve::check_unknown_modules;
use tyc_resolve::{check_unknown_modules_with, ImportVettingContext};
use tyc_syntax::preprocess::{
    expand_gather_blocks, expand_go_calls, expand_inline_question_ops, expand_lazy_imports,
    expand_multiline_guards, expand_pipes, expand_question_ops, expand_with_chains, preprocess,
};

use crate::commands::util::{apply_strictness, collect_dty_files, collect_ty_files};
use crate::config::TyphonConfig;

/// Arguments for `tyc check`.
#[derive(Args, Debug)]
pub struct CheckArgs {
    /// `.ty` files or directories to check.
    #[arg(value_name = "PATH", default_value = ".")]
    pub paths: Vec<PathBuf>,

    /// Validate `.dty` stub files against the runtime modules they describe.
    ///
    /// In v1 this flag only validates that every `.dty` file parses, resolves,
    /// and type-checks cleanly — the full stubtest-style runtime comparison
    /// (which would import the implementation module and diff its symbols) is
    /// deferred. The flag is recognised today so CI configurations are
    /// forward-compatible.
    #[arg(long)]
    pub stubs: bool,

    /// Suppress the trailing "checked N file(s)" success line. Errors and
    /// warnings still surface verbatim. Internal — used by `tyc run` to
    /// gate VM execution behind a successful check without spamming stdout.
    #[arg(skip)]
    pub quiet_success: bool,
}

pub fn run(args: CheckArgs) -> Result<()> {
    // Load strictness config from `typhon.toml`, anchoring the search to the
    // first checked path (or CWD when none is provided) so that
    // `tyc check path/to/project` uses that project's config, not the caller's.
    let config_start = args
        .paths
        .first()
        .map(|p| {
            if p.is_file() {
                p.parent()
                    .map(|d| d.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."))
            } else {
                p.clone()
            }
        })
        .unwrap_or_else(|| PathBuf::from("."));
    let (config, has_project_config, project_root) = match TyphonConfig::load(&config_start) {
        // `TyphonConfig::load` walks ancestors searching for `typhon.toml`,
        // so the returned path may live in a parent of `config_start`. The
        // venv-introspection step below needs the *project root* (the
        // directory containing `typhon.toml`), not the subdir the user ran
        // `tyc check` against.
        Ok(Some((path, cfg))) => {
            // `path.parent()` returns `Some("")` for a bare
            // `"typhon.toml"` (the relative case when `tyc check` is
            // invoked from the project root). An empty PathBuf is a
            // valid Rust value but downstream subprocess spawns that
            // pass it as `current_dir` fail with ENOENT — degrade to
            // `"."` so the parent's CWD is inherited instead.
            let root = path
                .parent()
                .map(|p| p.to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| PathBuf::from("."));
            (cfg, true, root)
        }
        Ok(None) => (TyphonConfig::default(), false, PathBuf::from(".")),
        Err(e) => return Err(miette!("{e}")),
    };

    let mut diags = Diagnostics::new();
    let mut file_count = 0usize;
    let mut db = TycDatabase::new();

    // FINDINGS #79: build the set of dotted module names contained in the
    // project so the per-file unknown-module check can resolve sibling
    // imports without falsely flagging them. `extra_modules` adds
    // typhon.toml-declared dependencies; users who manage deps directly
    // through `uv`/`pip` can still bypass the check by listing the
    // package in `typhon.toml`.
    //
    // When no `typhon.toml` is found the check is skipped entirely: a
    // standalone `.ty` file being checked outside a project context should
    // not be penalised for importing third-party packages that happen not to
    // be listed anywhere.
    let project_modules = collect_project_modules(&args.paths, &config.project.src);
    let mut extra_modules: Vec<String> = config
        .dependencies
        .keys()
        .chain(config.dev_dependencies.keys())
        .cloned()
        .collect();
    // Venv-aware fallback: read top-level import names from each
    // installed distribution's `.dist-info/top_level.txt` (and `RECORD`
    // as a backup) and add ONLY the top-level packages that belong to a
    // distribution declared in `[dependencies]` / `[dev-dependencies]`.
    //
    // This catches the dist/import mismatch case where hyphen->underscore
    // normalisation isn't enough — e.g. the PyPI dist
    // `agent-framework-core` exposes the top-level Python module
    // `agent_framework`, `beautifulsoup4` exposes `bs4` — without
    // letting any *undeclared* locally-installed package quietly pass
    // `tyc check`. Reproducibility wins: if a colleague clones the
    // repo, `tyc check` reports the same set of unknown_module
    // diagnostics regardless of what extra packages happen to be in
    // their venv.
    if has_project_config {
        let declared: std::collections::HashSet<String> = config
            .dependencies
            .keys()
            .chain(config.dev_dependencies.keys())
            .map(|k| pep503_normalise(k))
            .collect();
        extra_modules.extend(top_level_imports_from_venv(&project_root, &declared));
    }
    extra_modules.sort();
    extra_modules.dedup();
    // Build the import-vetting HashSets exactly once; the per-file
    // unknown-module pass reuses them via `check_unknown_modules_with`.
    let vetting_ctx = tyc_resolve::ImportVettingContext::new(&project_modules, &extra_modules);

    // Project-wide shape registry: dotted module name → public class /
    // function shapes the module exports. Built once before the per-file
    // check loop so cross-module constructor / method arity validation
    // can resolve a `from foo import ApiClient` against the `foo`
    // module's `class ApiClient: api_key: str` declaration.
    //
    // `.dty` stubs and `.ty` source contribute on equal footing — both
    // declare the Typhon-level surface, and the stub is the source of
    // truth when both forms exist (the latter inserted second by
    // iteration loses the `or_insert` race intentionally).
    // `path_to_dotted` matches `src_root` by single-component
    // equality, so pass the *basename* of the configured src
    // directory. A `[project] src = "app/src"` setting would
    // otherwise fall back to a basename-only dotted name and break
    // cross-module shape lookup. FINDINGS — copilot review of
    // v0.2.0.
    let src_root_name = std::path::Path::new(&config.project.src)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&config.project.src)
        .to_owned();
    let mut shape_map = collect_project_shapes(&args.paths, &src_root_name);

    // Venv-introspection enrichment: shell to the project's
    // `.venv/bin/python` and ask `inspect.signature` for the real
    // parameter list of every third-party class / free function the
    // project imports. Without this, `from agent_framework import
    // Agent; Agent(name="x")` would silently pass `tyc check` even
    // when `Agent.__init__` requires a `client` kwarg — the runtime
    // would catch it with `TypeError: missing 1 required positional
    // argument: 'client'`. Skipped when no `typhon.toml` is found
    // (standalone-file mode) or when no allow-listed top-level
    // packages exist.
    if has_project_config {
        let project_module_set: std::collections::HashSet<String> =
            project_modules.iter().cloned().collect();
        let allowed_top_level: std::collections::HashSet<String> = extra_modules
            .iter()
            .map(|m| m.split('.').next().unwrap_or(m).to_owned())
            .collect();
        crate::venv_signatures::enrich_project_shapes_with_venv(
            &args.paths,
            &project_root,
            &project_module_set,
            allowed_top_level,
            &mut shape_map,
        );
    }
    let project_shapes = std::sync::Arc::new(shape_map);

    for root in &args.paths {
        for path in collect_ty_files(root)? {
            file_count += 1;

            let source = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    diags.push_error(TycError::io(path.display().to_string(), &e));
                    continue;
                }
            };

            let file_diags = check_file_with_imports(
                &mut db,
                path.display().to_string(),
                source.clone(),
                &project_shapes,
            );
            diags.extend(file_diags);

            // Run comptime + purity analysis to match what `tyc build` would
            // reject. Without this pass, CI pipelines running only `tyc check`
            // would silently accept `@pure` violations and unsatisfied
            // required env vars that production builds catch. The work
            // duplicates `tyc build`'s phase 2/3 setup; salsa caches the
            // preprocess so it's cheap on warm runs.
            let analysis_diags = run_analysis_passes(&path.display().to_string(), &source);
            diags.extend(analysis_diags);

            // FINDINGS #79: vet imports against stdlib + project + deps.
            // Skip when no `typhon.toml` was found — standalone files checked
            // outside a project context should not be penalised for importing
            // third-party packages that are not listed in any config.
            if has_project_config {
                let module_diags =
                    run_unknown_module_check(&path.display().to_string(), &source, &vetting_ctx);
                diags.extend(module_diags);
            }
        }

        // `--stubs`: parse + type-check every `.dty` stub, then compare its
        // surface API against the sibling `.ty` (or `.py`) implementation
        // module.  Mismatches are reported through the standard diagnostics
        // channel so CI treats them like any other check error.
        if args.stubs {
            for path in collect_dty_files(root)? {
                file_count += 1;
                let source = match std::fs::read_to_string(&path) {
                    Ok(s) => s,
                    Err(e) => {
                        diags.push_error(TycError::io(path.display().to_string(), &e));
                        continue;
                    }
                };
                let file_diags = check_file_with_imports(
                    &mut db,
                    path.display().to_string(),
                    source.clone(),
                    &project_shapes,
                );
                diags.extend(file_diags);

                // Find the implementation module by stem.  Prefer a sibling
                // `.ty` (Typhon source) over a `.py` (raw Python) so users
                // can stub a Typhon module without also writing Python.
                let impl_path = path
                    .with_extension("ty")
                    .exists()
                    .then(|| path.with_extension("ty"))
                    .or_else(|| {
                        let py = path.with_extension("py");
                        py.exists().then_some(py)
                    });
                let Some(impl_path) = impl_path else {
                    // No implementation file located — stub stands alone, no
                    // diff possible.  This is intentional for downstream
                    // libraries the user is describing for type-checkers.
                    continue;
                };
                let impl_source = match std::fs::read_to_string(&impl_path) {
                    Ok(s) => s,
                    Err(e) => {
                        diags.push_error(TycError::io(impl_path.display().to_string(), &e));
                        continue;
                    }
                };

                match diff_stub_against_impl(&source, &impl_source) {
                    Ok(findings) => {
                        for finding in findings {
                            let label = match finding.kind {
                                StubTestKind::MissingInImpl => "missing in implementation",
                                StubTestKind::MissingInStub => "missing in stub",
                                StubTestKind::SignatureMismatch => "signature mismatch",
                            };
                            diags.push_error(TycError::stub_mismatch(
                                format!("{label}: {}", finding.message),
                                path.display().to_string(),
                                source.clone(),
                                0,
                                1,
                            ));
                        }
                    }
                    Err(e) => {
                        diags.push_error(TycError::stub_mismatch(
                            format!("could not diff stub against implementation: {e}"),
                            path.display().to_string(),
                            source.clone(),
                            0,
                            1,
                        ));
                    }
                }
            }
        }
    }

    // Apply strictness rules (e.g. promote unused-import warnings to errors).
    let mut diags = apply_strictness(diags, &config);
    // Remove duplicate diagnostics that can arise when multiple files share
    // an error root (e.g. a repeated definition across passes).
    diags.dedup();

    // Emit warnings regardless of whether there are errors.
    for warn in diags.warnings() {
        eprintln!("{:?}", miette::Report::new_boxed(Box::new(warn.clone())));
    }

    if diags.has_errors() {
        for err in diags.errors() {
            eprintln!("{:?}", miette::Report::new_boxed(Box::new(err.clone())));
        }
        return Err(miette!(
            "{} error(s) in {} file(s)",
            diags.error_count(),
            file_count
        ));
    }

    if !args.quiet_success {
        if diags.warning_count() > 0 {
            println!(
                "checked {} file(s) — {} warning(s)",
                file_count,
                diags.warning_count()
            );
        } else {
            println!("checked {} file(s) — no errors", file_count);
        }
    }
    Ok(())
}

/// Run comptime evaluation and purity verification on a single source file.
///
/// These passes also run inside `tyc build`; lifting them up to `tyc check`
/// closes the documented CI hole where `@pure` violations and missing
/// `[env] required` variables only fail at build time. Any non-comptime,
/// non-purity error has already been reported by `check_file`, so this
/// helper deliberately swallows preprocess / parse failures (they would
/// surface a second time otherwise).
fn run_analysis_passes(path: &str, source: &str) -> Diagnostics {
    let mut diags = Diagnostics::new();
    let expanded = expand_for_check(source);
    let prep = preprocess(&expanded);
    let module = match tyc_syntax::parse_module(&prep.python_source) {
        Ok(p) => p.into_syntax(),
        Err(_) => return diags,
    };

    // Pass `comptime_functions` so `comptime def` calls dispatch
    // correctly in check the same way they do in build (FINDINGS #48).
    let (_, comptime_diags) = evaluate_comptime_with_functions(
        &module,
        &prep.comptime_bindings,
        &prep.comptime_functions,
    );
    diags.extend(comptime_diags);

    let purity_findings = analyse_purity(&module, false);
    let purity_diags = purity_diagnostics(&purity_findings, path, source);
    diags.extend(purity_diags);

    diags
}

/// Normalise a PyPI distribution name per PEP 503: replace runs of
/// `[-_.]+` with a single `-` and lowercase. `tyc` uses this to match
/// `[dependencies]` keys against `.dist-info` directory names regardless
/// of casing or separator drift (`Agent-Framework-Core`,
/// `agent_framework_core`, `agent.framework.core` all normalise the
/// same).
fn pep503_normalise(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_sep = false;
    for ch in name.chars() {
        if ch == '-' || ch == '_' || ch == '.' {
            if !prev_sep && !out.is_empty() {
                out.push('-');
                prev_sep = true;
            }
        } else {
            out.push(ch.to_ascii_lowercase());
            prev_sep = false;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}

/// Scan the project's local virtualenv for installed Python
/// distributions and return the top-level import names each one
/// provides — restricted to distributions actually declared in
/// `[dependencies]` / `[dev-dependencies]`. Used by `tyc check` to vet
/// imports whose package name differs from the PyPI distribution name
/// (e.g. `agent-framework-core` -> `agent_framework`,
/// `beautifulsoup4` -> `bs4`).
///
/// Looks for a venv at `<project_root>/.venv` — the default `uv` /
/// `tyc sync` location — and reads `<dist>-<ver>.dist-info/top_level.txt`
/// from each declared distribution's metadata. If `top_level.txt` is
/// absent (newer wheels often omit it) we fall back to scanning the
/// `RECORD` manifest for top-level `<pkg>/__init__.py` paths.
///
/// `declared` is a set of PEP 503-normalised distribution names; only
/// `.dist-info` directories whose dist-name normalises into that set
/// contribute import roots. This keeps `tyc check` reproducible across
/// machines: a developer with extra packages in their local venv
/// won't accidentally pass imports that would fail on a fresh clone.
///
/// Returns an empty list when no venv exists.
fn top_level_imports_from_venv(
    project_root: &std::path::Path,
    declared: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if declared.is_empty() {
        return out;
    }
    let venv_root = project_root.join(".venv");
    if !venv_root.is_dir() {
        return out;
    }
    // `site-packages` lives under `lib/pythonX.Y` on POSIX,
    // `Lib/site-packages` on Windows. Probe both.
    let mut site_dirs: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(venv_root.join("lib")) {
        for entry in entries.flatten() {
            let p = entry.path().join("site-packages");
            if p.is_dir() {
                site_dirs.push(p);
            }
        }
    }
    let win_site = venv_root.join("Lib").join("site-packages");
    if win_site.is_dir() {
        site_dirs.push(win_site);
    }
    for site in site_dirs {
        let Ok(entries) = std::fs::read_dir(&site) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let dir_name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let Some(stem) = dir_name.strip_suffix(".dist-info") else {
                continue;
            };
            // `<dist>-<version>.dist-info` — the dist-name is the part
            // before the LAST `-<version>` segment. Strip the trailing
            // `-…` component (versions are PEP 440 and don't contain `/`
            // or whitespace, so trimming the rightmost `-` block is safe).
            let dist_name = stem.rsplit_once('-').map(|(d, _)| d).unwrap_or(stem);
            let normalised = pep503_normalise(dist_name);
            if !declared.contains(&normalised) {
                continue;
            }
            // Prefer top_level.txt — one package name per line.
            let tlt = path.join("top_level.txt");
            if let Ok(content) = std::fs::read_to_string(&tlt) {
                for line in content.lines() {
                    let line = line.trim();
                    // Filter out empty lines and namespace-package markers.
                    // top_level.txt may also list dotted paths for
                    // sub-packages; we only need the root component.
                    if line.is_empty() {
                        continue;
                    }
                    let root = line.split('/').next().unwrap_or(line);
                    let root = root.split('.').next().unwrap_or(root);
                    if !root.is_empty() && !root.starts_with('_') {
                        out.push(root.to_owned());
                    }
                }
                continue;
            }
            // Fallback: derive top-level packages from RECORD.
            // Each entry is `<path>,<hash>,<size>`; the path may be
            // double-quoted (PEP 376) if it contains commas, may be a
            // relative `../bin/script` for installed-outside-site
            // files, or absolute. We accept only paths that point at
            // a Python module (`<name>.py`) or at a Python source file
            // inside a top-level directory (`<name>/.../*.py(i)`) — the
            // narrower filter avoids picking up shipped `bin/`,
            // `docs/`, or `tests/` directories.
            if let Ok(record) = std::fs::read_to_string(path.join("RECORD")) {
                for line in record.lines() {
                    let raw = line.split(',').next().unwrap_or("").trim();
                    let path_field = raw.trim_matches('"');
                    if path_field.is_empty()
                        || path_field.starts_with("../")
                        || path_field.starts_with("..\\")
                        || path_field.starts_with('/')
                        || path_field.starts_with('\\')
                    {
                        continue;
                    }
                    let head = path_field.split('/').next().unwrap_or(path_field);
                    if head.is_empty()
                        || head == "."
                        || head == ".."
                        || head.starts_with('_')
                        || head.ends_with(".dist-info")
                        || head.ends_with(".data")
                    {
                        continue;
                    }
                    let import_name = if let Some(stem) = head.strip_suffix(".py") {
                        // Top-level single-file module (`foo.py`).
                        stem
                    } else if head.ends_with(".pyi") {
                        // Pure-stub package — uncommon but valid.
                        head.strip_suffix(".pyi").unwrap_or(head)
                    } else if path_field.contains('/')
                        && (path_field.ends_with(".py") || path_field.ends_with(".pyi"))
                    {
                        // Python source nested under a top-level
                        // directory — treat the directory as a package.
                        head
                    } else {
                        continue;
                    };
                    if !import_name.is_empty() && !import_name.starts_with('_') {
                        out.push(import_name.to_owned());
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Collect the dotted-name form of every `.ty` file under the given
/// `paths`. Used by [`run_unknown_module_check`] to vet sibling imports.
///
/// Names are derived by stripping the prefix up to the configured
/// source-root component (`project.src` in `typhon.toml`, default
/// `"src"`) and joining the remaining segments with `.`. With the
/// default, `src/main.ty` becomes `"main"`; `src/pkg/sub.ty` becomes
/// `"pkg.sub"`; an `__init__.ty` collapses to its parent package name.
/// Files outside any source-root directory fall back to their basename
/// so single-file scripts still resolve correctly.
///
/// `src_root` is the basename of the configured source directory
/// (e.g. `"src"` or `"app"`); it's matched by component-equality
/// against the path. (Copilot review on PR #68, file check.rs:303.)
/// Walk every `.ty` and `.dty` file under each search root and build
/// the dotted-name → public-shape registry that
/// [`check_file_with_imports`] consults. The dotted name uses the same
/// `ty_path_to_dotted` helper as the unknown-import vetting pass, so a
/// `from foo.bar import X` import lookup hits the file at
/// `<src_root>/foo/bar.{ty,dty}`. `.dty` stubs win on duplicates
/// (preferred since they're the authored Typhon surface).
fn collect_project_shapes(
    paths: &[PathBuf],
    src_root: &str,
) -> std::collections::HashMap<String, tyc_db::ModuleShapes> {
    let mut shapes: std::collections::HashMap<String, tyc_db::ModuleShapes> =
        std::collections::HashMap::new();
    // Stubs first so the `.ty` insertion below skips them — `.dty`
    // is the source of truth for the public Typhon surface.
    for root in paths {
        if let Ok(files) = collect_dty_files(root) {
            for file in files {
                let dotted = ty_path_to_dotted(&file, src_root);
                if let Ok(text) = std::fs::read_to_string(&file) {
                    shapes.entry(dotted).or_insert_with(|| {
                        extract_shapes_for_path(&file.display().to_string(), &text)
                    });
                }
            }
        }
        if let Ok(files) = collect_ty_files(root) {
            for file in files {
                let dotted = ty_path_to_dotted(&file, src_root);
                if shapes.contains_key(&dotted) {
                    continue;
                }
                if let Ok(text) = std::fs::read_to_string(&file) {
                    shapes.insert(
                        dotted,
                        extract_shapes_for_path(&file.display().to_string(), &text),
                    );
                }
            }
        }
    }
    shapes
}

fn collect_project_modules(paths: &[PathBuf], src_root: &str) -> Vec<String> {
    let mut modules: Vec<String> = Vec::new();
    for root in paths {
        if let Ok(files) = collect_ty_files(root) {
            for file in files {
                let dotted = ty_path_to_dotted(&file, src_root);
                if !modules.contains(&dotted) {
                    modules.push(dotted);
                }
            }
        }
    }
    modules
}

fn ty_path_to_dotted(path: &std::path::Path, src_root: &str) -> String {
    crate::commands::util::path_to_dotted(path, src_root)
}

/// FINDINGS #79: run `check_unknown_modules` for one source file. Parses
/// the file through the same preprocess pipeline used elsewhere so
/// Typhon-specific keywords (`val`, `var`, `lazy import`, …) are stripped
/// before the AST walk. Returns the warnings only — errors at the
/// preprocess / parse layer have already been surfaced by `check_file`.
fn run_unknown_module_check(path: &str, source: &str, ctx: &ImportVettingContext) -> Diagnostics {
    let expanded = expand_for_check(source);
    let prep = preprocess(&expanded);
    let module = match tyc_syntax::parse_module(&prep.python_source) {
        Ok(p) => p.into_syntax(),
        Err(_) => return Diagnostics::new(),
    };
    // AST node ranges are offsets into the *preprocessed* Python source,
    // so the diagnostic must render against `prep.python_source` for the
    // span labels to line up. Rendering against the original Typhon
    // source would print out-of-bounds labels for files that exercise
    // preprocess rewrites (`interface`, `impl`, `guard`, `lazy import`,
    // …). (Copilot review on PR #68, file check.rs:337.)
    check_unknown_modules_with(path, &prep.python_source, &module, ctx)
}

/// Shared preprocess pipeline used by every "parse the .ty source for a
/// secondary check pass" call site inside `tyc check`. Centralising the
/// chain keeps `run_unknown_module_check`, `run_analysis_passes`, and
/// `parse_for_diff` in sync with `tyc_db::check_file` / `tyc build` —
/// without this the three call sites diverged on which expansion passes
/// they ran, and a file using a feature recognised by only some of the
/// chains would silently skip downstream diagnostics. (Copilot review
/// on PR #68, file check.rs:332.)
fn expand_for_check(source: &str) -> String {
    expand_question_ops(&expand_inline_question_ops(&expand_pipes(
        &expand_with_chains(&expand_go_calls(&expand_gather_blocks(
            &expand_multiline_guards(&expand_lazy_imports(source)),
        ))),
    )))
}

/// Run the full preprocess + parse pipeline on `source` and return the
/// resulting Python AST.  Used by the stub diff so that Typhon-specific
/// syntax (`val`, `var`, `model`, `interface`, `extend`, sugar passes)
/// is normalised before comparing.
fn parse_for_diff(source: &str) -> Result<ruff_python_ast::ModModule> {
    let expanded = expand_for_check(source);
    let prep = preprocess(&expanded);
    tyc_syntax::parse_module(&prep.python_source)
        .map(|p| p.into_syntax())
        .map_err(|e| miette!("parse error: {e}"))
}

fn diff_stub_against_impl(
    stub_source: &str,
    impl_source: &str,
) -> Result<Vec<tyc_emit::StubTestFinding>> {
    let stub = parse_for_diff(stub_source)?;
    let imp = parse_for_diff(impl_source)?;
    Ok(compare_modules(&stub, &imp))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_ty(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn pep503_normalisation_matches_spec() {
        // Confirms the dist-name normalisation used to filter venv
        // distributions against `[dependencies]` keys agrees with
        // PEP 503: replace runs of `[-_.]+` with `-`, lowercase the
        // result. Same shape PyPA / pip / uv apply, so a key of
        // `Agent_Framework.Core` matches a wheel filed under
        // `agent-framework-core`.
        assert_eq!(
            pep503_normalise("Agent-Framework-Core"),
            "agent-framework-core"
        );
        assert_eq!(
            pep503_normalise("agent_framework_core"),
            "agent-framework-core"
        );
        assert_eq!(
            pep503_normalise("agent.framework.core"),
            "agent-framework-core"
        );
        assert_eq!(
            pep503_normalise("Agent_Framework.Core"),
            "agent-framework-core"
        );
        assert_eq!(pep503_normalise("beautifulsoup4"), "beautifulsoup4");
        assert_eq!(pep503_normalise("__leading"), "leading");
    }

    #[test]
    fn check_passes_valid_ty_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "ok.ty", "let x: int = 1\n");
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: false,
            quiet_success: false,
        };
        run(args).unwrap();
    }

    #[test]
    fn check_reports_type_mismatch_as_error() {
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "bad.ty", "let x: int = \"hello\"\n");
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: false,
            quiet_success: false,
        };
        assert!(run(args).is_err(), "type mismatch should be an error");
    }

    #[test]
    fn check_passes_nullable_annotation() {
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "nullable.ty", "let x: str? = None\n");
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: false,
            quiet_success: false,
        };
        run(args).unwrap();
    }

    #[test]
    fn check_reports_val_reassignment_as_error() {
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "immut.ty", "let x: int = 1\nx = 2\n");
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: false,
            quiet_success: false,
        };
        assert!(run(args).is_err(), "val reassignment should be an error");
    }

    #[test]
    fn check_reports_frozen_field_write_as_error() {
        // End-to-end guard for the user-visible flow: `tyc check` must
        // surface `tyc::frozen_assign` instead of letting the program
        // build cleanly and crash at runtime with `FrozenInstanceError`.
        let tmp = tempfile::tempdir().unwrap();
        write_ty(
            tmp.path(),
            "frozen.ty",
            "\
class Identity frozen:
    name: str

let i: Identity = Identity(name=\"Alice\")
i.name = \"Bob\"
",
        );
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: false,
            quiet_success: false,
        };
        assert!(
            run(args).is_err(),
            "writes to a frozen class field must fail `tyc check`"
        );
    }

    #[test]
    fn check_stubs_passes_for_matching_stub_and_impl() {
        let tmp = tempfile::tempdir().unwrap();
        // The stub and implementation expose the same function with the
        // same parameter names — diff should be empty.
        write_ty(tmp.path(), "lib.dty", "def hello(name: str) -> str: ...\n");
        write_ty(
            tmp.path(),
            "lib.ty",
            "def hello(name: str) -> str:\n    return name\n",
        );
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: true,
            quiet_success: false,
        };
        run(args).unwrap();
    }

    #[test]
    fn check_stubs_fails_when_function_missing_in_impl() {
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "lib.dty", "def hello(name: str) -> str: ...\n");
        write_ty(
            tmp.path(),
            "lib.ty",
            "def goodbye(name: str) -> str:\n    return name\n",
        );
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: true,
            quiet_success: false,
        };
        assert!(
            run(args).is_err(),
            "stub declares hello() which impl does not — should error"
        );
    }

    #[test]
    fn check_stubs_fails_when_param_names_differ() {
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "lib.dty", "def hello(name: str) -> str: ...\n");
        write_ty(
            tmp.path(),
            "lib.ty",
            "def hello(other_name: str) -> str:\n    return other_name\n",
        );
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: true,
            quiet_success: false,
        };
        assert!(
            run(args).is_err(),
            "parameter rename should produce a signature-mismatch error"
        );
    }

    #[test]
    fn check_standalone_file_skips_unknown_module() {
        // A standalone `.ty` file without a `typhon.toml` must not fire
        // `tyc::unknown_module` for third-party imports — the user is
        // checking a file outside a project context.
        let tmp = tempfile::tempdir().unwrap();
        write_ty(
            tmp.path(),
            "script.ty",
            "\
import requests

def fetch(url: str) -> str:
    let r = requests.get(url)
    return r.text
",
        );
        // No `typhon.toml` written — this is the standalone-file case.
        let args = CheckArgs {
            paths: vec![tmp.path().join("script.ty")],
            stubs: false,
            quiet_success: false,
        };
        // The check should pass (no unknown_module error) because there is
        // no project config to anchor the dependency check to.
        run(args).unwrap();
    }

    #[test]
    fn check_introspects_third_party_class_constructor_arity() {
        // Reproduces the original bug: `from agent_framework import
        // Agent; Agent(name="x", tools=[])` passed `tyc check`
        // because `agent_framework` had no `.dty` stub. With venv
        // introspection on, the checker recovers `Agent.__init__`'s
        // real signature from the installed package and the
        // missing required `client` kwarg fires `tyc::arg_count`.
        //
        // Requires a Python 3 on PATH; skip silently otherwise so CI
        // runners without Python don't fail the suite.
        if crate::venv_signatures::which_python3_for_test().is_none() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        // Fake third-party package, importable via Python's CWD-based
        // sys.path[0] entry. Lives at the project root so the
        // subprocess Python spawned by `enrich_project_shapes_with_venv`
        // finds it. The package name must match a [dependencies] key.
        let pkg = tmp.path().join("fake_introspect_pkg");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("__init__.py"),
            "\
class Agent:
    def __init__(self, *, name, client, tools=None):
        pass
",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("typhon.toml"),
            "[project]\n\
             name = \"t\"\nversion = \"0.0.0\"\nsrc = \"src\"\nout = \"out\"\n\
             [python]\ntarget = \"3.13\"\n\
             [dependencies]\nfake_introspect_pkg = \"*\"\n",
        )
        .unwrap();
        write_ty(
            &src,
            "main.ty",
            "from fake_introspect_pkg import Agent\n\
             let a: Agent = Agent(name=\"x\", tools=[])\n",
        );
        // The introspection helper sets its own subprocess CWD to
        // `project_root`, so Python's `sys.path[0]` picks up
        // `fake_introspect_pkg` without touching the process-global
        // CWD that other parallel tests share.
        let project_root = tmp.path();
        let mut shape_map: std::collections::HashMap<String, tyc_db::ModuleShapes> =
            std::collections::HashMap::new();
        let project_module_set: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let allowed: std::collections::HashSet<String> =
            ["fake_introspect_pkg".to_owned()].into_iter().collect();
        crate::venv_signatures::enrich_project_shapes_with_venv(
            std::slice::from_ref(&src),
            project_root,
            &project_module_set,
            allowed,
            &mut shape_map,
        );
        // The fake_introspect_pkg shape must now carry `Agent` with
        // field_order = ["name", "client", "tools"] and
        // field_defaults = {"tools"}.
        let shapes = shape_map
            .get("fake_introspect_pkg")
            .expect("introspection should produce shapes for the fake package");
        let agent_shape = shapes
            .class_shapes
            .get("Agent")
            .expect("Agent class should be introspected");
        assert_eq!(agent_shape.field_order, vec!["name", "client", "tools"]);
        assert!(!agent_shape.field_defaults.contains("name"));
        assert!(!agent_shape.field_defaults.contains("client"));
        assert!(agent_shape.field_defaults.contains("tools"));
    }

    #[test]
    fn check_with_project_config_warns_unknown_module() {
        // When a `typhon.toml` is present, undeclared third-party imports
        // produce a `tyc::unknown_module` WARNING (not an error).
        // `run()` returns Ok on warnings, so we test via the resolver helper
        // directly to confirm the warning fires in project context.
        let source = "import requests\n";
        let expanded = expand_question_ops(&expand_inline_question_ops(&expand_pipes(
            &expand_with_chains(source),
        )));
        let module = tyc_syntax::parse_module(&expanded).unwrap().into_syntax();
        let diags = check_unknown_modules("t.ty", source, &module, &[], &[]);
        assert!(
            diags.warnings().iter().any(|w| {
                matches!(w, TycError::UnknownModule { module, .. } if module == "requests")
            }),
            "check_unknown_modules must warn about undeclared third-party import; got {:?}",
            diags.warnings()
        );
    }
}
