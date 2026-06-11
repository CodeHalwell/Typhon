//! `tyc check` — parse, resolve, and type-check without emitting code.
//!
//! Runs the full Phase-1 pipeline: pre-process → parse → resolve
//! (scopes + `val`/`var` enforcement + unknown-name diagnostics) → type
//! check (nominal types, non-null narrowing, argument-count and
//! argument-type checks). Diagnostics are rendered via miette.

use std::path::PathBuf;

use clap::Args;
use miette::{miette, Result};

use tyc_analyse::{
    analyse_empty_collection_bindings, analyse_is_literal_comparisons,
    analyse_mutable_default_params, analyse_purity, analyse_secret_literal_bindings,
    analyse_typing_alias_annotations, evaluate_comptime_with_functions, purity_diagnostics,
};
use tyc_db::{check_file_with_imports, extract_shapes_for_path, TycDatabase};
use tyc_diagnostics::{sanitised_named_source_for, Diagnostics, SanitisedDiagnostic, TycError};
use tyc_emit::{compare_modules, StubTestKind};
#[cfg(test)]
use tyc_resolve::check_unknown_modules;
use tyc_resolve::{check_unknown_modules_with, ImportVettingContext};
use tyc_syntax::preprocess::{
    expand_gather_blocks, expand_go_calls, expand_inline_question_ops, expand_lazy_imports,
    expand_multiline_guards, expand_pipes, expand_question_ops, expand_typed_let_unpack,
    expand_with_chains, preprocess,
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

    /// Build to a temporary directory and run Astral's `ty` over the emitted
    /// Python as a typeshed-backed second-stage checker (the same behaviour
    /// as `[checker] external = "ty"`, for a single invocation). Requires
    /// `ty` on `PATH`.
    #[arg(long)]
    pub with_ty: bool,
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
            .map(|k| tyc_venv::pep503_normalise(k))
            .collect();
        extra_modules.extend(tyc_venv::top_level_imports_from_venv(
            &project_root,
            &declared,
        ));
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
    // Aggregate `pub *` package facades into their __init__ shape so a
    // downstream `from <pkg> import X` resolves through the facade the
    // same way it does at build time. Without this, cross-module flow
    // through a facade loses sealed-union variant lists, class shapes,
    // function signatures, and interfaces — surfacing as misleading
    // diagnostics like `tyc::missing_return` on a match the
    // exhaustiveness checker accepted. (Bug 2 from v0.9.0 stress.)
    {
        let mut all_paths: Vec<PathBuf> = Vec::new();
        for root in &args.paths {
            if let Ok(files) = collect_ty_files(root) {
                all_paths.extend(files);
            }
            // `.dty` stubs participate in the shape map on equal footing
            // with `.ty` (stubs win), so the aggregation pass needs to
            // see them too — otherwise a `pub *` facade implemented as
            // `__init__.dty` or a sibling `.dty` module wouldn't be
            // re-exported. (Copilot PR review on check.rs.)
            if let Ok(files) = collect_dty_files(root) {
                all_paths.extend(files);
            }
        }
        crate::commands::util::aggregate_pub_star_shapes(
            &mut shape_map,
            &all_paths,
            &src_root_name,
        );
    }

    // Resolved source directory for the `tyc::stdlib_module_shadow`
    // gating below. We canonicalise once here so the per-file check
    // does one syscall (the file's parent) instead of two.
    // Canonicalisation fails when the path doesn't exist; in that case
    // we fall back to the joined-but-unresolved path so the check
    // degrades gracefully (it'll just under-match rather than panic).
    let src_dir = project_root.join(&config.project.src);
    let src_dir_canon = src_dir.canonicalize().unwrap_or(src_dir);

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
    let mut unintrospectable_deps: Vec<String> = Vec::new();
    if has_project_config {
        let project_module_set: std::collections::HashSet<String> =
            project_modules.iter().cloned().collect();
        let allowed_top_level: std::collections::HashSet<String> = extra_modules
            .iter()
            .map(|m| m.split('.').next().unwrap_or(m).to_owned())
            .collect();
        unintrospectable_deps = tyc_venv::enrich_project_shapes_with_venv(
            &args.paths,
            &project_root,
            &project_module_set,
            allowed_top_level,
            &mut shape_map,
        );
    }
    // Surface declared dependencies that couldn't be introspected so their
    // skipped third-party checks are visible rather than silently passing.
    // Gated on `!quiet_success` so `tyc run`'s internal check gate stays
    // quiet. `"error"` severity escalates to a check failure below.
    let unintrospectable_fatal = !args.quiet_success
        && tyc_venv::report_unintrospectable_dependencies(
            &unintrospectable_deps,
            &config.strictness.unintrospectable_dependency,
        );
    let project_shapes = std::sync::Arc::new(shape_map);

    for root in &args.paths {
        let ty_files = collect_ty_files(root)?;

        // FINDINGS #39: when the user points `tyc check` at a single
        // `.dty` stub file (e.g. `tyc check lib.dty`), the
        // `.ty`-only collector silently returns an empty list and the
        // run reports "checked 0 file(s)" with no explanation. Detect
        // that case here and check the stub directly — this is the
        // sensible default for a single-file invocation, even without
        // `--stubs` (the flag scopes to *recursive stub discovery*
        // inside a directory tree).
        let direct_dty: Vec<PathBuf> = if ty_files.is_empty()
            && root.is_file()
            && root.extension().is_some_and(|e| e == "dty")
        {
            vec![root.clone()]
        } else {
            Vec::new()
        };

        // Pre-load every file into memory so the pub-star collision
        // pass can see the whole source tree at once, and the per-file
        // loop below pulls from this cache rather than hitting the
        // filesystem twice. `io_errors` carries (path, error) pairs
        // for paths that failed to read so the per-file loop can
        // emit the same `TycError::io` diagnostic it always did.
        // Review thread copilot on PR #147.
        let mut all_sources: Vec<(PathBuf, String)> = Vec::new();
        let mut io_errors: std::collections::HashMap<PathBuf, std::io::Error> =
            std::collections::HashMap::new();
        for path in ty_files.iter().chain(direct_dty.iter()) {
            match std::fs::read_to_string(path) {
                Ok(s) => all_sources.push((path.clone(), s)),
                Err(e) => {
                    io_errors.insert(path.clone(), e);
                }
            }
        }
        let source_lookup: std::collections::HashMap<PathBuf, String> = all_sources
            .iter()
            .map(|(p, s)| (p.clone(), s.clone()))
            .collect();

        // B28: detect `pub *` name collisions and `pub *` misplaced
        // outside `__init__.ty` across the whole file set BEFORE the
        // per-file check loop. Without this, name collisions only
        // surface during `tyc build` and CI gates running `tyc check`
        // silently pass.
        let (pub_star_errors, pub_star_advice) =
            super::build::detect_pub_star_diagnostics(&all_sources);
        for err in pub_star_errors {
            diags.push_error(err);
        }
        for adv in pub_star_advice {
            diags.push_warning(adv);
        }

        for path in ty_files.into_iter().chain(direct_dty.into_iter()) {
            file_count += 1;

            let source = match source_lookup.get(&path) {
                Some(s) => s.clone(),
                None => {
                    let err = io_errors
                        .get(&path)
                        .expect("source absent from cache must have an io_errors entry");
                    diags.push_error(TycError::io(path.display().to_string(), err));
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

            // R2-4: warn if the module name collides with a Python
            // stdlib module. Only fires when a `typhon.toml` is present —
            // standalone-file checks skip the warning (no `build/` will
            // be emitted, so the runtime collision can't happen). The
            // check also only fires for files at the top of the source
            // tree — nested files lower into a sub-package whose
            // emitted `.py` is not on `sys.path` and so cannot
            // intercept stdlib imports.
            if has_project_config {
                if let Some(warning) = check_stdlib_module_shadow(&path, &source, &src_dir_canon) {
                    diags.push_warning(warning);
                }
            }

            // Run comptime + purity + (optionally) unknown-module
            // diagnostics in one pass so the preprocess+parse cycle is
            // only done once per file instead of once per analysis. On
            // 100-LOC files this is a small win, but on larger trees
            // it adds up — each preprocess walks the full source and
            // each `parse_module` call rebuilds the ruff AST.
            let analysis_diags = run_secondary_passes(
                &path.display().to_string(),
                &source,
                has_project_config.then_some(&vetting_ctx),
                config.strictness.allow_secret_comptime,
            );
            diags.extend(analysis_diags);
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

                // R2-4: same stdlib-name shadow check for `.dty` stubs
                // (the implementation module they describe is emitted
                // under the same stem). Gated on `has_project_config`
                // to match the `.ty` branch above — a standalone
                // `tyc check --stubs path/to/types.dty` outside a
                // project context emits no `build/` so the runtime
                // collision can't happen. PR #129 copilot review.
                if has_project_config {
                    if let Some(warning) =
                        check_stdlib_module_shadow(&path, &source, &src_dir_canon)
                    {
                        diags.push_warning(warning);
                    }
                }

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
                            // Push as a warning so that `apply_strictness`
                            // can promote it to an error (`stub-check =
                            // "error"`, the default), keep it as a warning
                            // (`"warn"`), or drop it (`"off"`).
                            diags.push_warning(TycError::stub_mismatch(
                                format!("{label}: {}", finding.message),
                                path.display().to_string(),
                                source.clone(),
                                0,
                                1,
                            ));
                        }
                    }
                    Err(e) => {
                        diags.push_warning(TycError::stub_mismatch(
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

    render_diagnostics(&diags);

    if diags.has_errors() {
        return Err(miette!(
            "{} error{} ({} unique code{}) in {} file{}{}",
            diags.error_count(),
            if diags.error_count() == 1 { "" } else { "s" },
            unique_code_count(diags.errors()),
            if unique_code_count(diags.errors()) == 1 {
                ""
            } else {
                "s"
            },
            file_count,
            if file_count == 1 { "" } else { "s" },
            if diags.warning_count() > 0 {
                format!(" and {} warning(s)", diags.warning_count())
            } else {
                String::new()
            },
        ));
    }

    // `[strictness] unintrospectable-dependency = "error"` escalates a
    // skipped third-party check into a hard failure (already reported above).
    if unintrospectable_fatal {
        return Err(miette!(
            "declared dependencies could not be introspected (see message above); \
             failing because `[strictness] unintrospectable-dependency = \"error\"`"
        ));
    }

    // `--with-ty`: the Typhon check passed; now build to a throwaway
    // directory and run Astral's `ty` over the emitted Python as a
    // typeshed-backed second stage. `tyc check` is normally emit-free, so
    // this opts into a one-shot build (the build's own `--with-ty` hook
    // runs `ty` and re-attributes diagnostics to the `.ty` source).
    if args.with_ty {
        if !has_project_config {
            eprintln!(
                "warning: --with-ty needs a project (typhon.toml) to build and check; skipping"
            );
        } else {
            let td = tempfile::tempdir()
                .map_err(|e| miette!("failed to create temp build dir for --with-ty: {e}"))?;
            crate::commands::build::run(crate::commands::build::BuildArgs {
                path: project_root.clone(),
                out: Some(td.path().to_path_buf()),
                no_format: true,
                check: false,
                no_sync: true,
                with_ty: true,
            })?;
        }
    }

    if !args.quiet_success {
        if file_count == 0 {
            // FINDINGS #39: a silent "checked 0 file(s)" leaves the
            // user wondering whether the run actually did anything.
            // Print an actionable hint pointing at what we looked for
            // and at the `--stubs` flag (the recursive stub-discovery
            // path), so the user sees that a directory of `.dty` files
            // without any `.ty` siblings isn't picked up by default.
            let display_paths: Vec<String> =
                args.paths.iter().map(|p| p.display().to_string()).collect();
            let joined = if display_paths.is_empty() {
                ".".to_owned()
            } else {
                display_paths.join(", ")
            };
            println!(
                "no checkable files in {joined}: looked for `.ty` source files. \
                 To check stubs recursively, run `tyc check --stubs {joined}`; \
                 to check a single `.dty` stub pass it directly."
            );
        } else if diags.warning_count() > 0 {
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

/// Render every diagnostic in `diags`, grouping by source file (so CI
/// logs cluster related errors instead of interleaving by phase) and
/// printing a per-code tally + `tyc explain` hint at the end. The
/// previous renderer fired errors in the order they were collected,
/// which scattered findings across files on multi-file projects.
fn render_diagnostics(diags: &Diagnostics) {
    use miette::Diagnostic;

    // Group by `(severity, file)` so a warning in foo.ty doesn't get
    // sandwiched between errors in bar.ty. Severity buckets stay in
    // a fixed order: warnings first (advisory, easy to skim past),
    // errors last (immediately visible above the summary line).
    let warnings_by_file = group_by_file(diags.warnings());
    let errors_by_file = group_by_file(diags.errors());

    // FINDINGS #32: hand each diagnostic to a `SanitisedDiagnostic`
    // wrapper before rendering so synthetic preprocess output
    // (`class __typhon_impl_Foo(object):`, `from typhon_runtime
    // import …`, the `?`-operator scaffolding) doesn't leak into the
    // user-facing source listing. The wrapper preserves every other
    // miette field — code, severity, labels, help — so the diagnostic
    // reads identically apart from the cleaned source pane.
    //
    // Sanitise once per file: every diagnostic in `items` carries the
    // same embedded source, so computing it per-diagnostic is
    // O(n_diags × file_size) work. Compute once for the file group and
    // clone the cleaned `NamedSource` into each wrapper.
    let render_group = |label: &str, groups: &[(String, Vec<&TycError>)]| {
        for (file, items) in groups {
            eprintln!("── {} in {} ──", label, file);
            let cached = items.first().and_then(|d| sanitised_named_source_for(d));
            for d in items {
                let wrapped = match cached.clone() {
                    Some(src) => SanitisedDiagnostic::wrap_with_source((*d).clone(), src),
                    None => SanitisedDiagnostic::wrap((*d).clone()),
                };
                eprintln!("{:?}", miette::Report::new_boxed(Box::new(wrapped)));
            }
        }
    };
    render_group("warnings", &warnings_by_file);
    render_group("errors", &errors_by_file);

    // Per-code tally + `tyc explain` hint. Only emitted when the file
    // produced at least one diagnostic — silence on clean runs.
    if diags.error_count() + diags.warning_count() == 0 {
        return;
    }
    let mut counts: std::collections::BTreeMap<String, (usize, usize)> =
        std::collections::BTreeMap::new();
    for e in diags.errors() {
        let code = e.code().map(|c| c.to_string()).unwrap_or_default();
        counts.entry(code).or_default().0 += 1;
    }
    for w in diags.warnings() {
        let code = w.code().map(|c| c.to_string()).unwrap_or_default();
        counts.entry(code).or_default().1 += 1;
    }
    eprintln!();
    eprintln!("── summary ──");
    for (code, (errs, warns)) in &counts {
        let display_code = if code.is_empty() {
            "(uncoded)"
        } else {
            code.as_str()
        };
        match (errs, warns) {
            (0, w) => eprintln!("  {} warning(s): {}", w, display_code),
            (e, 0) => eprintln!("  {} error(s):   {}", e, display_code),
            (e, w) => eprintln!("  {} error(s) + {} warning(s): {}", e, w, display_code),
        }
    }
    // Surface the `tyc explain` workflow: most users don't realise the
    // CLI bundles the docs catalogue, so the URL in the rendered
    // miette output goes unclicked.
    let first_code = counts
        .keys()
        .find(|c| !c.is_empty())
        .map(|c| c.trim_start_matches("tyc::").to_owned());
    if let Some(code) = first_code {
        eprintln!();
        eprintln!(
            "  hint: run `tyc explain {}` for a full explanation (and try `tyc explain --list` to browse all codes).",
            code
        );
    }
}

/// Group a diagnostic slice by primary file path, preserving the
/// first-seen order so the output keeps a deterministic shape.
/// Diagnostics whose file path can't be recovered (a few of the older
/// path-less variants, like the generic shape used by `Comptime`)
/// fall into a synthetic "(no location)" bucket so they still
/// surface instead of being silently dropped.
fn group_by_file(items: &[TycError]) -> Vec<(String, Vec<&TycError>)> {
    let mut order: Vec<String> = Vec::new();
    let mut buckets: std::collections::HashMap<String, Vec<&TycError>> =
        std::collections::HashMap::new();
    for item in items {
        let file = extract_path_hint(item).unwrap_or_else(|| "(no location)".to_owned());
        if !buckets.contains_key(&file) {
            order.push(file.clone());
        }
        buckets.entry(file).or_default().push(item);
    }
    order
        .into_iter()
        .map(|f| {
            let items = buckets.remove(&f).unwrap_or_default();
            (f, items)
        })
        .collect()
}

/// Heuristic: pull a file path out of a `TycError`'s `Debug` form by
/// looking for the `path:` field every variant embeds. Robust enough
/// for grouping (worst case: diagnostics with the same path land in
/// separate buckets if the Debug format changes, which only affects
/// the visual grouping — render is unchanged).
fn extract_path_hint(err: &TycError) -> Option<String> {
    let dbg = format!("{:?}", err);
    // Variants like `WrongArgCount { name: "f", src: NamedSource { name: "./src/b.ty", … } }`
    // have a `name:` field *outside* the NamedSource that holds the
    // callee or symbol name — picking the first match would group
    // every WrongArgCount under "f" instead of the file path.
    // Anchor on the prefix `NamedSource { name:` so we always pull the
    // path embedded inside the source-code field.
    let ns_key = "NamedSource { name: \"";
    if let Some(start) = dbg.find(ns_key) {
        let rest = &dbg[start + ns_key.len()..];
        if let Some(end) = rest.find('"') {
            return Some(rest[..end].to_owned());
        }
    }
    // Fallback: `path: "..."` for variants without a NamedSource
    // (`Io`, the generic shapes).
    let key = "path: \"";
    if let Some(start) = dbg.find(key) {
        let rest = &dbg[start + key.len()..];
        if let Some(end) = rest.find('"') {
            return Some(rest[..end].to_owned());
        }
    }
    None
}

/// Count unique diagnostic codes across an error slice — fed into the
/// summary line so the user gets "3 errors (2 unique codes)" rather
/// than just a raw count. Helps recognise repeated patterns.
fn unique_code_count(items: &[TycError]) -> usize {
    use miette::Diagnostic;
    let mut codes: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in items {
        let code = item.code().map(|c| c.to_string()).unwrap_or_default();
        codes.insert(code);
    }
    codes.len()
}

/// Python 3.13 stdlib top-level module names. A project `.ty` file
/// whose stem matches one of these will be emitted as `build/<name>.py`,
/// and when `build/` is on `sys.path` (the default for `python
/// build/main.py`) any transitive stdlib `import <name>` will resolve
/// to the user module instead of the stdlib — leading to mystifying
/// `ImportError`s blamed on innocent stdlib packages. R2-4.
///
/// The list is intentionally restricted to top-level modules whose
/// names collide with names users naturally pick for application
/// modules. Subpackages (`urllib.parse`, `xml.etree`) are excluded
/// because a top-level `.ty` file with that name is impossible.
const STDLIB_TOP_LEVEL: &[&str] = &[
    "abc",
    "argparse",
    "array",
    "ast",
    "asyncio",
    "atexit",
    "audioop",
    "base64",
    "bdb",
    "bisect",
    "builtins",
    "bz2",
    "calendar",
    "cmath",
    "cmd",
    "code",
    "codecs",
    "codeop",
    "collections",
    "colorsys",
    "compileall",
    "concurrent",
    "configparser",
    "contextlib",
    "contextvars",
    "copy",
    "copyreg",
    "cProfile",
    "csv",
    "ctypes",
    "curses",
    "dataclasses",
    "datetime",
    "dbm",
    "decimal",
    "difflib",
    "dis",
    "doctest",
    "email",
    "encodings",
    "enum",
    "errno",
    "faulthandler",
    "fcntl",
    "filecmp",
    "fileinput",
    "fnmatch",
    "fractions",
    "ftplib",
    "functools",
    "gc",
    "getopt",
    "getpass",
    "gettext",
    "glob",
    "graphlib",
    "gzip",
    "hashlib",
    "heapq",
    "hmac",
    "html",
    "http",
    "imaplib",
    "importlib",
    "inspect",
    "io",
    "ipaddress",
    "itertools",
    "json",
    "keyword",
    "linecache",
    "locale",
    "logging",
    "lzma",
    "mailbox",
    "marshal",
    "math",
    "mimetypes",
    "mmap",
    "modulefinder",
    "multiprocessing",
    "netrc",
    "numbers",
    "operator",
    "optparse",
    "os",
    "pathlib",
    "pdb",
    "pickle",
    "pickletools",
    "pkgutil",
    "platform",
    "plistlib",
    "poplib",
    "posix",
    "posixpath",
    "pprint",
    "profile",
    "pstats",
    "pty",
    "pwd",
    "py_compile",
    "pyclbr",
    "pydoc",
    "queue",
    "quopri",
    "random",
    "re",
    "readline",
    "reprlib",
    "resource",
    "rlcompleter",
    "runpy",
    "sched",
    "secrets",
    "select",
    "selectors",
    "shelve",
    "shlex",
    "shutil",
    "signal",
    "site",
    "smtplib",
    "socket",
    "socketserver",
    "sqlite3",
    "ssl",
    "stat",
    "statistics",
    "string",
    "stringprep",
    "struct",
    "subprocess",
    "symtable",
    "sys",
    "sysconfig",
    "syslog",
    "tabnanny",
    "tarfile",
    "telnetlib",
    "tempfile",
    "termios",
    "test",
    "textwrap",
    "threading",
    "time",
    "timeit",
    "tkinter",
    "token",
    "tokenize",
    "tomllib",
    "trace",
    "traceback",
    "tracemalloc",
    "tty",
    "turtle",
    "types",
    "typing",
    "unicodedata",
    "unittest",
    "urllib",
    "uu",
    "uuid",
    "venv",
    "warnings",
    "wave",
    "weakref",
    "webbrowser",
    "winreg",
    "winsound",
    "wsgiref",
    "xdrlib",
    "xml",
    "xmlrpc",
    "zipapp",
    "zipfile",
    "zipimport",
    "zlib",
    "zoneinfo",
];

/// True when `name` is the top-level name of a Python stdlib module
/// that would be shadowed by an emitted `build/<name>.py`.
pub(crate) fn stdlib_top_level_contains(name: &str) -> bool {
    STDLIB_TOP_LEVEL.contains(&name)
}

/// Emit a `stdlib_module_shadow` warning if `path`'s stem collides
/// with a stdlib module name. The warning points at byte offset 0 of
/// the file — the filename itself is the offending token, and the
/// help text tells the user how to rename. R2-4.
/// Byte offset of the start of the 0-based `line_idx` line in
/// `source`. Mirrors the `line_offset` helper in `build.rs` so the
/// `pub_star_outside_init` advice anchors at the same column on both
/// commands. Returns `source.len()` when the line index falls past the
/// end so the rendered diagnostic still has a valid span.
fn pub_star_line_offset(source: &str, line_idx: usize) -> usize {
    let mut offset = 0usize;
    let mut current = 0usize;
    for (i, b) in source.bytes().enumerate() {
        if current == line_idx {
            offset = i;
            break;
        }
        if b == b'\n' {
            current += 1;
            offset = i + 1;
        }
    }
    if current < line_idx {
        return source.len();
    }
    offset
}

fn check_stdlib_module_shadow(
    path: &std::path::Path,
    source: &str,
    src_dir: &std::path::Path,
) -> Option<TycError> {
    let stem = path.file_stem()?.to_str()?;
    if !stdlib_top_level_contains(stem) {
        return None;
    }
    // Only fire when the file sits AT the top of the configured
    // source tree — a nested `src/indexer/tokenize.ty` lowers to
    // `build/indexer/tokenize.py`, which is not on `sys.path` and so
    // cannot intercept `import tokenize` from stdlib callers. The
    // shadow risk is real only when the emitted `build/<stem>.py`
    // would sit alongside `build/main.py`.
    //
    // We compare canonicalised paths (rather than basenames) so that
    // (a) `[project] src = "."` projects still get the warning for
    // their top-level `.ty` files, where `parent.file_name()` would
    // resolve to the project directory name and never literally
    // equal `"."`, and (b) a nested `src/sub/src/tokenize.ty`
    // doesn't false-positive just because its parent dir is also
    // named "src".
    let parent = path.parent()?;
    let parent_canon = parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf());
    if parent_canon != src_dir {
        return None;
    }
    Some(TycError::stdlib_module_shadow(
        stem.to_owned(),
        path.display().to_string(),
        source.to_owned(),
        0,
        0,
    ))
}

/// Run the secondary check passes — comptime evaluation, purity
/// verification, and (when a project config is present) the
/// unknown-module import vet — over a single preprocess + parse of
/// `source`.
///
/// These passes also run inside `tyc build`; lifting them up to
/// `tyc check` closes the documented CI hole where `@pure` violations
/// and missing `[env] required` variables only fail at build time.
/// Any non-comptime, non-purity error has already been reported by
/// `check_file`, so this helper deliberately swallows preprocess /
/// parse failures (they would surface a second time otherwise).
///
/// Passing `vetting_ctx = None` skips the unknown-module check — the
/// standalone-file flow (no `typhon.toml` found) suppresses that
/// diagnostic since the user isn't in a project context.
fn run_secondary_passes(
    path: &str,
    source: &str,
    vetting_ctx: Option<&ImportVettingContext>,
    allow_secret_comptime: bool,
) -> Diagnostics {
    let mut diags = Diagnostics::new();
    let expanded = expand_for_check(source);
    let prep = preprocess(&expanded);

    // `pub *` outside `__init__.ty` is a no-op with confusing intent.
    // Surface the advice in `tyc check` (mirroring `tyc build`) so CI
    // gates on the same shape regardless of which command runs first.
    // The diagnostic carries `severity(Advice)`; push it onto the
    // warnings vec rather than errors so the build doesn't fail and
    // the summary reads "n warning(s): tyc::pub_star_outside_init".
    if !prep.pub_star_lines.is_empty() {
        let is_init = std::path::Path::new(path)
            .file_name()
            .and_then(|f| f.to_str())
            .map(|s| s == "__init__.ty")
            .unwrap_or(false);
        if !is_init {
            for &line_idx in &prep.pub_star_lines {
                let offset = pub_star_line_offset(&expanded, line_idx);
                diags.push_warning(TycError::pub_star_outside_init(
                    path.to_owned(),
                    expanded.clone(),
                    offset,
                    5,
                ));
            }
        }
    }

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

    // Empty-literal binding lint (`tyc::empty_collection_no_annotation`):
    // a `let xs = []` with no annotation defaults to `list[Unknown]` and
    // silently swallows later element-type mismatches. The pass walks the
    // already-preprocessed module, so spans line up with `prep.python_source`.
    diags.extend(analyse_empty_collection_bindings(
        &module,
        path,
        &prep.python_source,
    ));

    // Typing-alias-in-annotation lint (`tyc::typing_alias_in_annotation`):
    // the `typing.List` / `typing.Dict` / `Optional` / `Union` aliases are
    // rejected on import but were silently accepted inside annotations as
    // forward-reference names. Walk every annotation and surface the same
    // migration advice.
    diags.extend(analyse_typing_alias_annotations(
        &module,
        path,
        &prep.python_source,
    ));

    // Mutable-default-parameter lint (`tyc::mutable_default_param`):
    // `def f(xs: list[int] = [])` shares ONE list across every defaulted
    // call — the classic Python footgun. Class fields already get the
    // default_factory rewrite; function parameters get this warning.
    diags.extend(analyse_mutable_default_params(
        &module,
        path,
        &prep.python_source,
    ));

    // `is` against a literal (`s is "x"`) compares identity, not value —
    // interpreter-dependent and CPython SyntaxWarns on it.
    diags.extend(analyse_is_literal_comparisons(
        &module,
        path,
        &prep.python_source,
    ));

    // Secret-literal lint (`tyc::contains_secret_literal`, inline form):
    // a `let API_TOKEN = "abc"` hard-codes a credential into the source.
    // The existing comptime path inside `tyc build` only caught
    // `comptime let X = env(...)`; this pass catches the plain-`let`
    // form so the check fires in `tyc check` too.
    diags.extend(analyse_secret_literal_bindings(
        &module,
        path,
        &prep.python_source,
        allow_secret_comptime,
    ));

    if let Some(ctx) = vetting_ctx {
        // AST node ranges are offsets into the *preprocessed* Python
        // source, so the diagnostic must render against
        // `prep.python_source` for the span labels to line up.
        // Rendering against the original Typhon source would print
        // out-of-bounds labels for files that exercise preprocess
        // rewrites (`interface`, `impl`, `guard`, `lazy import`, …).
        // (Copilot review on PR #68, file check.rs:337.)
        let module_diags = check_unknown_modules_with(path, &prep.python_source, &module, ctx);
        diags.extend(module_diags);
    }

    diags
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
        // `.dty` stubs are project modules too — `from stubs.fakelib
        // import X` against a `stubs/fakelib.dty` must not warn
        // `unknown_module` (the stub IS the module's Typhon surface).
        if let Ok(files) = collect_dty_files(root) {
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

/// Shared preprocess pipeline used by every "parse the .ty source for a
/// secondary check pass" call site inside `tyc check`. Centralising the
/// chain keeps `run_secondary_passes` and `parse_for_diff` in sync with
/// `tyc_db::check_file` / `tyc build` — without this the call sites
/// diverged on which expansion passes they ran, and a file using a
/// feature recognised by only some of the chains would silently skip
/// downstream diagnostics. (Copilot review on PR #68, file
/// check.rs:332.)
fn expand_for_check(source: &str) -> String {
    expand_question_ops(&expand_inline_question_ops(&expand_pipes(
        &expand_with_chains(&expand_go_calls(&expand_gather_blocks(
            &expand_multiline_guards(&expand_lazy_imports(&expand_typed_let_unpack(source))),
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
    fn stdlib_module_shadow_detects_collision() {
        // R2-4: project module named `types`, `ast`, `string` etc. must
        // be flagged because `build/<name>.py` will shadow the stdlib.
        assert!(stdlib_top_level_contains("types"));
        assert!(stdlib_top_level_contains("ast"));
        assert!(stdlib_top_level_contains("string"));
        assert!(stdlib_top_level_contains("io"));
        assert!(stdlib_top_level_contains("json"));
        assert!(stdlib_top_level_contains("dataclasses"));
    }

    #[test]
    fn stdlib_module_shadow_accepts_safe_names() {
        // Names that don't collide with stdlib must not fire.
        assert!(!stdlib_top_level_contains("lang_types"));
        assert!(!stdlib_top_level_contains("models"));
        assert!(!stdlib_top_level_contains("app"));
        assert!(!stdlib_top_level_contains("scheduler"));
    }

    #[test]
    fn stdlib_module_shadow_emits_warning_on_colliding_filename() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir(&src).unwrap();
        let src_canon = src.canonicalize().unwrap();
        let path = write_ty(&src, "types.ty", "pub class Foo:\n    x: int\n");
        let diag = check_stdlib_module_shadow(&path, "pub class Foo:\n    x: int\n", &src_canon);
        assert!(
            diag.is_some(),
            "expected a warning for a project module named `types` at the top of src/"
        );
    }

    #[test]
    fn stdlib_module_shadow_skips_disjoint_filename() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir(&src).unwrap();
        let src_canon = src.canonicalize().unwrap();
        let path = write_ty(&src, "lang_types.ty", "");
        let diag = check_stdlib_module_shadow(&path, "", &src_canon);
        assert!(diag.is_none());
    }

    #[test]
    fn stdlib_module_shadow_skips_nested_subpackage_file() {
        // `src/indexer/tokenize.ty` lowers to
        // `build/indexer/tokenize.py`, which is NOT on `sys.path` —
        // so it cannot intercept `import tokenize` from stdlib
        // callers. The shadow risk is real only for top-level files.
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let sub = src.join("indexer");
        std::fs::create_dir_all(&sub).unwrap();
        let src_canon = src.canonicalize().unwrap();
        let path = write_ty(&sub, "tokenize.ty", "");
        let diag = check_stdlib_module_shadow(&path, "", &src_canon);
        assert!(
            diag.is_none(),
            "nested file's emitted .py is not on sys.path; no shadow risk"
        );
    }

    #[test]
    fn stdlib_module_shadow_fires_when_src_equals_project_root() {
        // `[project] src = "."` puts every top-level `.ty` directly
        // at the project root. The pre-PR-138-comments check
        // compared `parent.file_name()` to the basename "."
        // (which would be the project dir's name) and never fired —
        // suppressing a real shadow case. Resolved-path comparison
        // restores the warning.
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().to_path_buf();
        let project_canon = project.canonicalize().unwrap();
        let path = write_ty(&project, "types.ty", "pub class Foo:\n    x: int\n");
        let diag =
            check_stdlib_module_shadow(&path, "pub class Foo:\n    x: int\n", &project_canon);
        assert!(
            diag.is_some(),
            "expected a warning for `types.ty` at the project root when `src = \".\"`"
        );
    }

    #[test]
    fn stdlib_module_shadow_skips_false_positive_same_named_nested_dir() {
        // `src/indexer/src/tokenize.ty` — the parent dir's basename
        // is "src" and so is the configured src root. The
        // pre-PR-138-comments basename comparison would have
        // fired here. The resolved-path comparison correctly
        // suppresses, because the file isn't actually under the
        // configured source root.
        let tmp = tempfile::tempdir().unwrap();
        let real_src = tmp.path().join("src");
        let nested_src = real_src.join("indexer").join("src");
        std::fs::create_dir_all(&nested_src).unwrap();
        let real_src_canon = real_src.canonicalize().unwrap();
        let path = write_ty(&nested_src, "tokenize.ty", "");
        let diag = check_stdlib_module_shadow(&path, "", &real_src_canon);
        assert!(
            diag.is_none(),
            "a nested `src/` directory must not false-positive against the configured src root"
        );
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
            tyc_venv::pep503_normalise("Agent-Framework-Core"),
            "agent-framework-core"
        );
        assert_eq!(
            tyc_venv::pep503_normalise("agent_framework_core"),
            "agent-framework-core"
        );
        assert_eq!(
            tyc_venv::pep503_normalise("agent.framework.core"),
            "agent-framework-core"
        );
        assert_eq!(
            tyc_venv::pep503_normalise("Agent_Framework.Core"),
            "agent-framework-core"
        );
        assert_eq!(
            tyc_venv::pep503_normalise("beautifulsoup4"),
            "beautifulsoup4"
        );
        assert_eq!(tyc_venv::pep503_normalise("__leading"), "leading");
    }

    #[test]
    fn check_passes_valid_ty_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "ok.ty", "let x: int = 1\n");
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: false,
            quiet_success: false,
            with_ty: false,
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
            with_ty: false,
        };
        assert!(run(args).is_err(), "type mismatch should be an error");
    }

    #[test]
    fn check_accepts_direct_dty_file_without_stubs_flag() {
        // FINDINGS #39: `tyc check lib.dty` used to silently report
        // "checked 0 file(s)" because the `.ty`-only collector skipped
        // the stub file. A single-file `.dty` invocation now resolves
        // to a direct stub check.
        let tmp = tempfile::tempdir().unwrap();
        let dty = tmp.path().join("lib.dty");
        std::fs::write(&dty, "def f(x: int) -> int: ...\n").unwrap();
        let args = CheckArgs {
            paths: vec![dty.clone()],
            stubs: false,
            quiet_success: false,
            with_ty: false,
        };
        run(args).expect("direct .dty check should succeed");
    }

    #[test]
    fn check_passes_nullable_annotation() {
        let tmp = tempfile::tempdir().unwrap();
        write_ty(tmp.path(), "nullable.ty", "let x: str? = None\n");
        let args = CheckArgs {
            paths: vec![tmp.path().to_path_buf()],
            stubs: false,
            quiet_success: false,
            with_ty: false,
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
            with_ty: false,
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
            with_ty: false,
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
            with_ty: false,
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
            with_ty: false,
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
            with_ty: false,
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
            with_ty: false,
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
        if tyc_venv::which_python3_for_test().is_none() {
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
        tyc_venv::enrich_project_shapes_with_venv(
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
