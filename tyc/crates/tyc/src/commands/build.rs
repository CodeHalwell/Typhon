//! `tyc build` — full compilation pipeline.
//!
//! Runs: expand `?` operators → pre-process → parse → type-check →
//!       evaluate comptime → substitute literals → desugar → emit.
//! Writes `.py` files into the output directory, mirroring the source tree.

use std::collections::HashMap;
use std::path::PathBuf;

use clap::Args;
use miette::{miette, Result};
use tyc_analyse::{
    analyse_purity, collect_gatherable_async_fn_names, detect_missed_gathers,
    evaluate_comptime_with_functions, extract_builtin_extensions, load_profile_samples,
    parallel_opportunity_diagnostics, pgo_memoise_targets, purity_diagnostics, rewrite_auto_gather,
    rewrite_builtin_extension_calls_tracking, rewrite_parallel_comprehensions,
    rewrite_reduction_loops, shared_mut_across_tasks_diagnostics, substitute_comptime_literals,
    ProfileSample,
};
use tyc_db::{check_file_with_imports, extract_shapes_for_path, TycDatabase};
use tyc_desugar::{desugar_module_with, DesugarOptions};
use tyc_diagnostics::{Diagnostics, TycError};
use tyc_emit::{emit_python_with_source_for_target, emit_stub};
use tyc_format::format_source;
use tyc_syntax::preprocess::{
    compose_line_maps, expand_compound_question_headers, expand_compound_question_headers_mapped,
    expand_gather_blocks, expand_gather_blocks_mapped, expand_go_calls, expand_go_calls_mapped,
    expand_inline_question_ops, expand_inline_question_ops_mapped, expand_lazy_imports,
    expand_lazy_imports_mapped, expand_lazy_lets_mapped, expand_multiline_guards,
    expand_multiline_guards_mapped, expand_pipes, expand_pipes_mapped, expand_question_ops,
    expand_question_ops_mapped, expand_typed_let_unpack, expand_typed_let_unpack_mapped,
    expand_with_chains, expand_with_chains_mapped, line_byte_starts, preprocess, preprocess_mapped,
    LazyImport,
};

use crate::commands::util::{
    apply_strictness, collect_dty_files, collect_py_files, collect_ty_files,
};
use crate::config::TyphonConfig;

/// Arguments for `tyc build`.
#[derive(Args, Debug)]
pub struct BuildArgs {
    /// Project directory (defaults to current directory).
    #[arg(value_name = "PATH", default_value = ".")]
    pub path: PathBuf,

    /// Write output to this directory instead of the configured `out` dir.
    /// Relative paths are resolved against the project root.
    #[arg(long, short = 'o', value_name = "DIR")]
    pub out: Option<PathBuf>,

    /// Skip formatting the emitted Python.
    #[arg(long)]
    pub no_format: bool,

    /// Dry run: list which output files would be created or overwritten
    /// without writing anything. The full pipeline still runs so type
    /// errors continue to surface.
    #[arg(long)]
    pub check: bool,

    /// Skip the `uv sync` step. `pyproject.toml` is still merged so the
    /// next regular build picks the manifest up. Useful for fast
    /// iteration on `.ty` files when the project's `.venv` is already
    /// provisioned. Also honoured via `TYC_NO_SYNC=1`.
    #[arg(long)]
    pub no_sync: bool,

    /// After a successful build, run Astral's `ty` over the emitted Python
    /// as a typeshed-backed second-stage checker — the same behaviour as
    /// `[checker] external = "ty"` but for a single invocation. Requires
    /// `ty` on `PATH` (`pip install ty` / `uv tool install ty`).
    #[arg(long)]
    pub with_ty: bool,

    /// Enable level-1 optimisation for this invocation, as if
    /// `[optimise] level = 1` were set in `typhon.toml` — flips the default
    /// of `auto-memoise`, `auto-gather`, `auto-parallel`, and `pgo-memoise`
    /// to on. An explicit `[strictness]` entry for any of those still wins,
    /// so `-O` never overrides a knob you set by hand.
    #[arg(short = 'O', long = "optimise", visible_alias = "optimize")]
    pub optimise: bool,
}

pub fn run(args: BuildArgs) -> Result<()> {
    // A single `.ty` file is not a project root — without this guard the
    // path gets joined with `src/` and the user sees the baffling
    // "source directory 'foo.ty/src' does not exist".
    if args.path.is_file() {
        return Err(miette!(
            "tyc build expects a project directory (with typhon.toml), not a \
             single file. Run `tyc run --compile {}` to build-and-execute the \
             file via a throwaway scaffold, `tyc run {}` for the in-process \
             VM, or `tyc init` to start a project.",
            args.path.display(),
            args.path.display()
        ));
    }
    let project_root = args
        .path
        .canonicalize()
        .map_err(|e| miette!("cannot resolve path '{}': {}", args.path.display(), e))?;

    // Load typhon.toml, anchoring src/out to the directory that contains it
    // so that `tyc build` works correctly when invoked from a subdirectory.
    let (config_dir, config) = match TyphonConfig::load(&project_root) {
        Ok(Some((toml_path, cfg))) => {
            let dir = toml_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| project_root.clone());
            (dir, cfg)
        }
        Ok(None) => {
            eprintln!("warning: no typhon.toml found; using defaults");
            (project_root.clone(), TyphonConfig::default())
        }
        // Lift typed `ConfigError` variants into the structured
        // `TycError` catalog so config-load failures render the same
        // code/url/help styling as every other compiler diagnostic and
        // are discoverable via `tyc explain`. Anything we don't have a
        // dedicated variant for falls through to a plain miette message.
        Err(crate::config::ConfigError::InvalidClassDefault {
            path,
            value,
            allowed,
        }) => {
            let err = TycError::invalid_config_value("emit.class-default", value, allowed, path);
            return Err(miette::Report::new_boxed(Box::new(err)));
        }
        Err(crate::config::ConfigError::InvalidSeverity {
            path,
            key,
            value,
            allowed,
        }) => {
            let field = format!("strictness.{key}");
            let err = TycError::invalid_config_value(&field, value, allowed, path);
            return Err(miette::Report::new_boxed(Box::new(err)));
        }
        Err(crate::config::ConfigError::InvalidModelExtra {
            path,
            value,
            allowed,
        }) => {
            let err = TycError::invalid_config_value("emit.model-extra", value, allowed, path);
            return Err(miette::Report::new_boxed(Box::new(err)));
        }
        Err(crate::config::ConfigError::InvalidChecker {
            path,
            value,
            allowed,
        }) => {
            let err = TycError::invalid_config_value("checker.external", value, allowed, path);
            return Err(miette::Report::new_boxed(Box::new(err)));
        }
        Err(crate::config::ConfigError::InvalidOptimiseLevel { path, value }) => {
            let err = TycError::invalid_config_value(
                "optimise.level",
                value.to_string(),
                "0, 1".to_owned(),
                path,
            );
            return Err(miette::Report::new_boxed(Box::new(err)));
        }
        Err(crate::config::ConfigError::InvalidParallelBackend {
            path,
            value,
            allowed,
        }) => {
            let err =
                TycError::invalid_config_value("strictness.parallel-backend", value, allowed, path);
            return Err(miette::Report::new_boxed(Box::new(err)));
        }
        Err(e) => return Err(miette!("{e}")),
    };

    let src_dir = config_dir.join(&config.project.src);

    // Resolve --out relative to project_root so `tyc build path/to/proj -o build`
    // writes to `path/to/proj/build` rather than the caller's cwd.
    let out_dir = match args.out {
        Some(out) => {
            if out.is_absolute() {
                out
            } else {
                project_root.join(out)
            }
        }
        None => config_dir.join(&config.project.out),
    };

    let do_format = config.emit.format && !args.no_format;
    // PEP 810 (Python 3.15) ships native `lazy import` syntax with exactly
    // the deferred-until-first-use semantics Typhon's bespoke lowering
    // emulates. On a 3.15+ target, lower `lazy import ALIAS = MODULE` to the
    // native `lazy import MODULE as ALIAS` form instead of the runtime-helper
    // call. For 3.13 / 3.14 targets this is `false` and the emitted Python is
    // byte-identical to before (a hard regression requirement). The check
    // uses the full `(major, minor)` tuple so a future major bump still gates
    // correctly. `tyc check` / `tyc run` / the REPL are unaffected — only
    // `tyc build`'s emitted `.py` changes, and only on 3.15+.
    let native_lazy_imports = crate::config::parse_python_target(&config.python.target)
        .is_some_and(|major_minor| major_minor >= (3, 15));
    let check_mode = args.check;
    // Counter for `would write …` lines so the final summary is honest.
    let mut would_write_count: usize = 0;

    // Fail fast if any required env vars are missing (declared in [env] required).
    for var in &config.env.required {
        if std::env::var(var).is_err() {
            return Err(miette!(
                "required environment variable '{}' is not set \
                 (declared in [env] required in typhon.toml)",
                var
            ));
        }
    }

    if !src_dir.exists() {
        return Err(miette!(
            "source directory '{}' does not exist",
            src_dir.display()
        ));
    }

    let ty_files = collect_ty_files(&src_dir)?;

    if ty_files.is_empty() {
        println!("no .ty files found in '{}'", src_dir.display());
        return Ok(());
    }

    // Read every source file once; both phases reuse this buffer.
    let sources: Vec<(PathBuf, String)> = ty_files
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| miette!("cannot read '{}': {e}", path.display()))?;
            Ok((path, text))
        })
        .collect::<Result<_>>()?;

    // Auto-add `pydantic` to [dependencies] when any source uses `model X:`
    // declarations, since the emitter generates `from pydantic import …`
    // imports for those. Without this the build artefact crashes at import
    // time with ModuleNotFoundError when the user hasn't declared pydantic
    // explicitly.
    let mut config = config;
    // Resolve the optimise-gated strictness knobs to concrete bools now that
    // both the config's `[optimise] level` and the CLI `-O`/`--optimise` flag
    // are known. After this, `config.strictness.{auto_memoise,auto_gather,
    // auto_parallel,pgo_memoise}` are `Some(_)` and read with `.unwrap_or(false)`.
    // An explicit `[strictness]` entry always wins over the level default.
    config.resolve_optimise(args.optimise);
    if sources_use_model_keyword(&sources) && !config.dependencies.contains_key("pydantic") {
        config
            .dependencies
            .insert("pydantic".to_string(), "*".to_string());
    }

    // B3 stdlib-shadow warning: a top-level module named after a Python
    // stdlib module (`types.ty`, `ast.ty`, …) emits `<out>/types.py`,
    // which lands on `sys.path` ahead of the stdlib and shadows it —
    // producing baffling circular-import failures pointing at stdlib
    // internals rather than the user's file (R2-4). Non-fatal: the build
    // still succeeds; we only surface an actionable rename suggestion.
    //
    // Reuses the exact `tyc::stdlib_module_shadow` detection that
    // `tyc check` runs (top-level-only gating, `lang_<name>` rename
    // hint), so the two surfaces never drift. `tyc build` previously
    // never emitted this warning — only `tyc check` did.
    {
        let src_dir_canon = src_dir.canonicalize().unwrap_or_else(|_| src_dir.clone());
        for (path, source) in &sources {
            if let Some(warn) =
                crate::commands::check::check_stdlib_module_shadow(path, source, &src_dir_canon)
            {
                eprintln!("{:?}", miette::Report::new_boxed(Box::new(warn)));
            }
        }
    }

    // Bootstrap the Python environment before codegen: merge our owned
    // keys into pyproject.toml (preserving user-managed tables) and run
    // `uv sync` so `.venv` is ready when the user runs the emitted
    // `.py`. Failure of `uv sync` is downgraded to a warning — the
    // codegen output is useful regardless of whether the install
    // step resolved.
    //
    // `--no-sync` (or `TYC_NO_SYNC=1`) skips the `uv sync` step while
    // still merging the manifest, so stress harnesses and REPL-like
    // iteration don't pay the per-invocation reprovision cost.
    let skip_sync = args.no_sync || std::env::var_os("TYC_NO_SYNC").is_some_and(|v| v == "1");
    crate::commands::deps::bootstrap_python_env_with(&config_dir, &config, skip_sync)?;

    // Phase 1: type-check all files first and fail fast on errors.
    let mut db = TycDatabase::new();
    let mut all_phase1_diags = Diagnostics::new();

    // Build the project-wide shape registry once so cross-module
    // constructor / method arity checks fire on imported symbols.
    // Same machinery as `tyc check` — see `collect_project_shapes`
    // there for the dual `.dty`-then-`.ty` walk that gives stubs
    // priority.
    // Basename of the source directory — `path_to_dotted` matches
    // by single-component equality, so a `src = "app/src"` config
    // would otherwise fall through to basename-only dotted names
    // and break cross-module shape lookups. FINDINGS — copilot
    // review of v0.2.0.
    let src_root_owned = std::path::Path::new(&config.project.src)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&config.project.src)
        .to_owned();
    let src_root = src_root_owned.as_str();
    let mut project_shapes: std::collections::HashMap<String, tyc_db::ModuleShapes> =
        std::collections::HashMap::new();
    // `.dty` stubs alongside the source tree should win on name
    // collisions because they're the authored Typhon surface.
    if let Ok(dty) = crate::commands::util::collect_dty_files(&src_dir) {
        for file in dty {
            let dotted = crate::commands::util::path_to_dotted(&file, src_root);
            if let Ok(text) = std::fs::read_to_string(&file) {
                project_shapes
                    .entry(dotted)
                    .or_insert_with(|| extract_shapes_for_path(&file.to_string_lossy(), &text));
            }
        }
    }
    for (path, source) in &sources {
        let dotted = crate::commands::util::path_to_dotted(path, src_root);
        project_shapes
            .entry(dotted)
            .or_insert_with(|| extract_shapes_for_path(&path.to_string_lossy(), source));
    }
    // Aggregate `pub *` package facades into their __init__ shape so a
    // downstream `from <pkg> import X` resolves through the facade.
    // Matches the source-level injection the lower-down `pub *` loop
    // does, but applied to the type-check shape map. (Bug 2 from
    // v0.9.0 stress.)
    {
        // Include `.dty` stub paths (the shape map is seeded with them
        // first so a facade declared as `__init__.dty` or sibling `.dty`
        // must aggregate too). (Copilot PR review on build.rs.)
        let mut all_paths: Vec<PathBuf> = sources.iter().map(|(p, _)| p.clone()).collect();
        if let Ok(dty) = crate::commands::util::collect_dty_files(&src_dir) {
            all_paths.extend(dty);
        }
        crate::commands::util::aggregate_pub_star_shapes(&mut project_shapes, &all_paths, src_root);
    }
    // Seed compiler-bundled stubs (httpx, requests, …) for any module not
    // already shaped by the project — same as `tyc check`. Before venv
    // enrichment so the venv pass skips them and they're exempt from the
    // `unintrospectable-dependency` warning.
    tyc_db::seed_bundled_stubs(&mut project_shapes);
    // Venv-introspection enrichment: shell to the project's Python
    // and recover real signatures for every third-party class /
    // function the project imports. Without this, calls like
    // `Agent(name="x")` for a `class Agent(*, name, client, …)`
    // would build clean and crash at runtime with `TypeError:
    // missing 1 required positional argument: 'client'`. See the
    // `tyc_venv` crate for the implementation and the allow-list rules.
    // Skipped silently when no Python / venv is reachable — the worst case
    // is the existing behaviour.
    let unintrospectable_deps: Vec<String> = {
        let project_module_set: std::collections::HashSet<String> = sources
            .iter()
            .map(|(path, _)| crate::commands::util::path_to_dotted(path, src_root))
            .collect();
        // Same allow-list as `tyc check` and the LSP: declared dependency
        // names expanded through installed `.dist-info` metadata so packages
        // whose import root differs from the PyPI name (`beautifulsoup4` →
        // `bs4`) are introspected here too.
        let allowed_top_level = tyc_venv::allowed_top_level_from_project(&config_dir);
        tyc_venv::enrich_project_shapes_with_venv(
            std::slice::from_ref(&src_dir),
            &config_dir,
            &project_module_set,
            allowed_top_level,
            &mut project_shapes,
        )
    };
    // Surface declared dependencies that couldn't be introspected so their
    // skipped third-party checks are visible rather than silently passing.
    // `"error"` severity fails the build (already reported by the helper).
    if tyc_venv::report_unintrospectable_dependencies(
        &unintrospectable_deps,
        &config.strictness.unintrospectable_dependency,
    ) {
        return Err(miette!(
            "declared dependencies could not be introspected (see message above); \
             failing because `[strictness] unintrospectable-dependency = \"error\"`"
        ));
    }
    // Wrap the registry in `Arc` so each per-file `ExternalShapes`
    // snapshot is a cheap refcount bump instead of an O(modules)
    // clone. FINDINGS — copilot review of v0.2.0.
    let project_shapes = std::sync::Arc::new(project_shapes);

    for (path, source) in &sources {
        let file_diags = check_file_with_imports(
            &mut db,
            path.to_string_lossy().into_owned(),
            source.clone(),
            &project_shapes,
        );
        all_phase1_diags.extend(file_diags);
    }

    // Apply strictness rules (e.g. promote unused-import warnings to errors).
    let all_phase1_diags = apply_strictness(all_phase1_diags, &config);

    // Emit warnings even when there are no errors so they are always visible.
    for warn in all_phase1_diags.warnings() {
        eprintln!("{:?}", miette::Report::new_boxed(Box::new(warn.clone())));
    }

    if all_phase1_diags.has_errors() {
        for err in all_phase1_diags.errors() {
            eprintln!("{:?}", miette::Report::new_boxed(Box::new(err.clone())));
        }
        return Err(miette!(
            "{} error(s) — fix type errors before building",
            all_phase1_diags.error_count()
        ));
    }

    // Phase 4 profile-guided optimisation: when `[strictness] pgo-memoise =
    // true`, load `typhon-profile.json` from the project root once and feed
    // it into each module's memoise-target computation. A missing file
    // yields an empty map (PGO is best-effort), so projects that have not
    // yet run `tyc profile` simply fall through to the explicit-decorator
    // path.
    let profile_samples: HashMap<String, ProfileSample> =
        if config.strictness.pgo_memoise.unwrap_or(false) {
            load_profile_samples(&config_dir.join("typhon-profile.json"))
        } else {
            HashMap::new()
        };

    // Phase 1.5: pre-collect every submodule's `pub`-marked names so
    // package `__init__.ty` files that opt in with `pub *` can have
    // those names re-exported through their emitted `__init__.py`.
    //
    // The map is keyed by package directory and holds `(submodule
    // basename, ordered pub names)` per non-`__init__` `.ty` file
    // that declared at least one `pub` symbol. Empty modules (no
    // `pub` markers) are skipped — there's nothing to re-export.
    // Sub-packages contribute via their own `__init__.ty` if the
    // user opts in there too, so aggregation cascades naturally.
    let mut pkg_pub_aggregation: std::collections::HashMap<PathBuf, Vec<(String, Vec<String>)>> =
        std::collections::HashMap::new();
    for (path, source) in &sources {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem == "__init__" {
            continue;
        }
        let expanded = expand_question_ops(&expand_inline_question_ops(
            &expand_compound_question_headers(&expand_pipes(&expand_with_chains(
                &expand_go_calls(&expand_gather_blocks(&expand_multiline_guards(
                    &expand_lazy_imports(&expand_typed_let_unpack(source)),
                ))),
            ))),
        ));
        let prep = preprocess(&expanded);
        if prep.pub_names.is_empty() {
            continue;
        }
        if let Some(dir) = path.parent() {
            pkg_pub_aggregation
                .entry(dir.to_path_buf())
                .or_default()
                .push((stem.to_owned(), prep.pub_names));
        }
    }

    // Phase 2: desugar and emit using the already-loaded source text.
    let mut emitted = 0usize;
    let mut needs_runtime = false;
    // R3 frontier: `pub *` name collisions are accumulated per-file
    // and surfaced as `tyc::pub_name_collision` diagnostics. Track the
    // total so the build can fail after the per-file loop instead of
    // silently emitting bad re-exports.
    let mut pub_star_collision_count: usize = 0;

    for (path, source) in &sources {
        // Expand Typhon syntactic sugar in order:
        //   1. `gather:` blocks lower to `asyncio.TaskGroup` / `asyncio.gather`,
        //   2. `go f(x)` lowers to `typhon_runtime.tasks.spawn(...)`,
        //   3. `with`-chains lower to a flat sequence of guarded unwraps,
        //   4. pipe operators rewrite `a |> f(b)` to `f(a, b)`,
        //   5. the `?` operator unwraps any remaining `Result`-typed calls.
        // After this the Python parser only sees standard Python plus the
        // Typhon line-prefix keywords (`val`/`var`/`model`/`impl`/`extend`/
        // `interface`/`unsafe`/`comptime`/`lazy`) stripped by `preprocess`.
        //
        // Note the lazy-import pass runs first so that `lazy import` lines are
        // handled before the other sugar passes see them. On a pre-3.15 target
        // `expand_lazy_imports` rewrites each `lazy import ALIAS = MODULE` into
        // a runtime-helper call here; on a 3.15+ target we instead run
        // `expand_lazy_lets` (which touches only `lazy let`), leaving the
        // `lazy import` line for the main `preprocess` pass below to convert to
        // `import MODULE as ALIAS` and record in `prep.lazy_imports`. The
        // recorded aliases drive the post-emit rewrite to native PEP 810
        // syntax further down. (This mirrors the VM / check paths, which
        // already use `expand_lazy_lets` + the `import … as …` rewrite.)
        //
        // Each pass is run through its `*_mapped` sibling and the resulting
        // `output line -> input line` tables are folded together, so we come
        // out of the chain holding a `preprocessed line -> .ty line` table.
        // Without it the `.py.map` sidecar written below would record line
        // numbers from the preprocessed buffer while claiming they are `.ty`
        // lines — wrong for every file using `?`, `gather:`, a `with`-chain,
        // `rescue`, pipes, or a typed unpack.
        let mut stage = expand_typed_let_unpack_mapped(source);
        if native_lazy_imports {
            chain_step(&mut stage, expand_lazy_lets_mapped);
        } else {
            chain_step(&mut stage, expand_lazy_imports_mapped);
        }
        chain_step(&mut stage, expand_multiline_guards_mapped);
        chain_step(&mut stage, expand_gather_blocks_mapped);
        chain_step(&mut stage, expand_go_calls_mapped);
        chain_step(&mut stage, expand_with_chains_mapped);
        chain_step(&mut stage, expand_pipes_mapped);
        chain_step(&mut stage, expand_compound_question_headers_mapped);
        // The inline `?` pass runs before the end-of-line `?` pass so
        // `Ok(f(x)?)`-shaped sub-expressions get lifted into temps
        // first. O17 / FINDINGS #66 / R3.13 / E9.
        chain_step(&mut stage, expand_inline_question_ops_mapped);
        chain_step(&mut stage, expand_question_ops_mapped);
        let (expanded, expanded_to_ty) = stage;
        let (mut prep, prep_to_expanded) = preprocess_mapped(&expanded);
        // The one table the `.py.map` writer needs: preprocessed line → `.ty`
        // line, both 0-based.
        let preprocessed_to_ty = compose_line_maps(&prep_to_expanded, &expanded_to_ty);

        // `pub *` wildcard re-export aggregation.
        //
        // The preprocessor records each `pub *` marker's line index in
        // `prep.pub_star_lines` and strips the line text. Here we:
        //   - For `__init__.ty`: aggregate sibling modules' `pub` names,
        //     detect cross-sibling collisions (`tyc::pub_name_collision`,
        //     which fails the build), and inject the synthesised
        //     `from .sibling import name1, name2` block at the marker
        //     line so the rest of the pipeline emits the imports.
        //   - For any other file: emit `tyc::pub_star_outside_init`
        //     advice — the wildcard re-export only has meaning at a
        //     package boundary.
        //
        // Aggregation includes BOTH direct .ty siblings AND direct
        // sub-packages (sub-directories containing their own
        // `__init__.ty`). When a sub-package's own `__init__.ty` uses
        // `pub *`, the recursion picks up its aggregated names too via
        // `effective_package_surface` (cycle-safe via a `visited` set
        // keyed on each package directory).
        //
        // Siblings are sorted by basename for deterministic re-export
        // ordering across runs and platforms (where filesystem
        // ordering varies).
        if !prep.pub_star_lines.is_empty() {
            let is_init = path
                .file_name()
                .and_then(|f| f.to_str())
                .map(|s| s == "__init__.ty")
                .unwrap_or(false);
            if !is_init {
                for &line_idx in &prep.pub_star_lines {
                    let offset = line_offset(&expanded, line_idx);
                    let advice = TycError::pub_star_outside_init(
                        path.to_string_lossy().into_owned(),
                        expanded.clone(),
                        offset,
                        5,
                    );
                    eprintln!("{:?}", miette::Report::new_boxed(Box::new(advice)));
                }
            } else if let Some(parent_dir) = path.parent() {
                // Collect direct .ty siblings (from the pre-computed
                // `pkg_pub_aggregation` map) and direct sub-packages
                // (walked separately for transitive expansion).
                let mut sibling_pubs: Vec<(String, Vec<String>)> = pkg_pub_aggregation
                    .get(parent_dir)
                    .cloned()
                    .unwrap_or_default();
                let mut subpackages_seen: std::collections::HashSet<PathBuf> =
                    std::collections::HashSet::new();
                let mut visited: std::collections::HashSet<PathBuf> =
                    std::collections::HashSet::new();
                visited.insert(parent_dir.to_path_buf());
                for (sib_path, _) in &sources {
                    let sib_parent = match sib_path.parent() {
                        Some(p) => p,
                        None => continue,
                    };
                    // Direct sub-package: a file whose grandparent is
                    // `parent_dir` and whose immediate parent has an
                    // `__init__.ty`. The sub-package name is the
                    // immediate parent's directory name.
                    if sib_parent.parent() == Some(parent_dir)
                        && subpackages_seen.insert(sib_parent.to_path_buf())
                    {
                        let sub_init = sib_parent.join("__init__.ty");
                        let has_init = sources.iter().any(|(p, _)| p == &sub_init);
                        if !has_init {
                            continue;
                        }
                        let sub_name = match sib_parent.file_name().and_then(|s| s.to_str()) {
                            Some(s) => s.to_owned(),
                            None => continue,
                        };
                        let names = effective_package_surface(sib_parent, &sources, &mut visited);
                        if !names.is_empty() {
                            sibling_pubs.push((sub_name, names));
                        }
                    }
                }
                // Deterministic re-export order by sibling basename.
                sibling_pubs.sort_by(|a, b| a.0.cmp(&b.0));
                // Detect collisions. Each collision bumps
                // `pub_star_collision_count` so the build fails after
                // the per-file loop instead of silently emitting bad
                // re-exports. `name_origin` starts with `__init__.ty`'s
                // own `pub` names so a same-named sibling export is
                // also reported (the package-local definition wins,
                // but the user gets told).
                let mut name_origin: std::collections::HashMap<String, String> = prep
                    .pub_names
                    .iter()
                    .map(|n| (n.clone(), "<package>".to_owned()))
                    .collect();
                let marker_line = *prep.pub_star_lines.first().unwrap_or(&0);
                let marker_offset = line_offset(&expanded, marker_line);
                let mut accepted_names: std::collections::HashSet<String> =
                    name_origin.keys().cloned().collect();
                for (sibling, names) in &sibling_pubs {
                    for name in names {
                        if let Some(prev) = name_origin.get(name) {
                            let err = TycError::pub_name_collision(
                                name.clone(),
                                prev.clone(),
                                sibling.clone(),
                                path.to_string_lossy().into_owned(),
                                expanded.clone(),
                                marker_offset,
                                5,
                            );
                            eprintln!("{:?}", miette::Report::new_boxed(Box::new(err)));
                            pub_star_collision_count += 1;
                        } else {
                            name_origin.insert(name.clone(), sibling.clone());
                            accepted_names.insert(name.clone());
                        }
                    }
                }
                // Build the import block. One `from .sibling import
                // names` per sibling that contributed at least one
                // non-colliding name, joined by `; ` so the whole
                // block stays on a single line (preserves source-map
                // line alignment with the original `pub *` marker).
                let import_pieces: Vec<String> = sibling_pubs
                    .iter()
                    .filter_map(|(sib, names)| {
                        let kept: Vec<&str> = names
                            .iter()
                            .filter(|n| {
                                // Only emit names actually accepted —
                                // those that didn't collide with a
                                // previous sibling or with __init__.ty.
                                name_origin.get(n.as_str()) == Some(&sib.clone())
                            })
                            .map(|s| s.as_str())
                            .collect();
                        if kept.is_empty() {
                            None
                        } else {
                            Some(format!("from .{} import {}", sib, kept.join(", ")))
                        }
                    })
                    .collect();
                if !import_pieces.is_empty() {
                    let import_line = import_pieces.join("; ");
                    prep.python_source =
                        replace_line(&prep.python_source, marker_line, &import_line);
                    // Append aggregated names to `prep.pub_names` so the
                    // desugar pass picks them up for `__all__`. Sort first —
                    // `accepted_names` is a `HashSet`, whose iteration order is
                    // nondeterministic, which made the synthesised `__all__`
                    // order flap between builds (non-reproducible output).
                    let mut sorted_accepted: Vec<&String> = accepted_names.iter().collect();
                    sorted_accepted.sort();
                    for name in sorted_accepted {
                        if !prep.pub_names.contains(name) && name != "<package>" {
                            prep.pub_names.push(name.clone());
                        }
                    }
                }
            }
        }

        let module = tyc_syntax::parse_module(&prep.python_source)
            .map(|p| p.into_syntax())
            .map_err(|e| miette!("parse error in '{}': {e}", path.display()))?;

        // Evaluate all `comptime` bindings and substitute their literals into
        // the AST before desugaring. `comptime def` functions registered by
        // the preprocessor are dispatchable from the binding RHSs.
        let (comptime_values, comptime_diags) = evaluate_comptime_with_functions(
            &module,
            &prep.comptime_bindings,
            &prep.comptime_functions,
        );

        // Phase 5.6 secret-literal lint: flag any comptime binding whose
        // name looks like a credential (KEY / TOKEN / PASSWORD / SECRET /
        // PASS / PWD, case-insensitive). The substituted value lands in
        // the emitted Python as a raw string literal — committing such
        // build output to a repository leaks the secret.
        //
        // Suppression knob: a `[strictness] allow-secret-comptime = true`
        // toggle in `typhon.toml` should silence this warning.
        // It is checked using `!config.strictness.allow_secret_comptime` below.
        if !config.strictness.allow_secret_comptime {
            for name in comptime_values.keys() {
                if secret_suffix(name).is_none() {
                    continue;
                }
                // Only fire when the RHS actually pulls from `env(...)` — a
                // hard-coded `comptime let API_KEY = "test"` isn't reading a
                // secret, just labelling a literal. Pull the actual env-var
                // key out of the source so the help text points at the right
                // identifier (the binding name and the env key often differ:
                // `comptime let API_KEY = env("MY_SERVICE_API_KEY")`).
                if let Some(env_key) = find_env_key_for_comptime_binding(source, name) {
                    let warn = TycError::contains_secret_literal(name.clone(), env_key);
                    eprintln!("{:?}", miette::Report::new_boxed(Box::new(warn)));
                }
            }
        }

        if comptime_diags.has_errors() {
            for err in comptime_diags.errors() {
                eprintln!("{:?}", miette::Report::new_boxed(Box::new(err.clone())));
            }
            return Err(miette!(
                "{} comptime error(s) in '{}'",
                comptime_diags.error_count(),
                path.display()
            ));
        }

        let module =
            substitute_comptime_literals(module, &comptime_values, &prep.comptime_functions);

        // Phase 3 purity analysis: every `@pure` / `@memo` function is verified
        // against the six-condition rule, and the desugarer is told which
        // functions to wrap in `@functools.cache`.
        let purity_findings =
            analyse_purity(&module, config.strictness.auto_memoise.unwrap_or(false));
        let purity_diags = purity_diagnostics(&purity_findings, &path.to_string_lossy(), source);
        if purity_diags.has_errors() {
            for err in purity_diags.errors() {
                eprintln!("{:?}", miette::Report::new_boxed(Box::new(err.clone())));
            }
            return Err(miette!(
                "{} purity error(s) in '{}'",
                purity_diags.error_count(),
                path.display()
            ));
        }
        let mut memoise_targets: Vec<String> = purity_findings
            .iter()
            .filter(|f| f.violation.is_none() && f.memoise)
            .map(|f| f.name.clone())
            .collect();

        // Phase 4 PGO: add every pure function whose observed call count
        // (from the loaded profile) meets the threshold. The matcher
        // requires an exact `<module>.<fn>` profile key for this file's
        // module so a hot `main.fib` doesn't accidentally promote a
        // coincidentally-named `util.fib` in another module. Names
        // already in `memoise_targets` are skipped so the desugarer
        // doesn't emit two cache decorators on the same definition.
        if !profile_samples.is_empty() {
            let pgo_candidates: Vec<String> = purity_findings
                .iter()
                .filter(|f| f.violation.is_none())
                .map(|f| f.name.clone())
                .collect();
            let module_name = python_module_name_from_path(path, &src_dir);
            let promoted = pgo_memoise_targets(
                &profile_samples,
                &module_name,
                &pgo_candidates,
                config.strictness.pgo_min_calls,
            );
            for name in promoted {
                if !memoise_targets.contains(&name) {
                    memoise_targets.push(name);
                }
            }
        }

        // Phase 4 auto-gather inference: when `[strictness] auto-gather = true`,
        // fold runs of independent awaits whose callees are `@gatherable`
        // module-level `async def`s into `asyncio.TaskGroup` blocks. The
        // user opts each callee in by writing the decorator; we never infer
        // gather-safety since same-module async fns may share I/O ordering
        // or other invisible state.  The desugar pass downstream notices
        // the qualified `asyncio.TaskGroup` reference and injects
        // `import asyncio` if it isn't already in scope, so no extra
        // wiring is needed here.
        let mut module = module;
        if config.strictness.auto_gather.unwrap_or(false) {
            // Surface runs that would have been gathered if every callee
            // carried `@gatherable`. Advice-only; doesn't block builds.
            // Print directly through miette so the rendered output shows
            // the [Advice] severity badge — these don't go through the
            // Diagnostics warnings list because Phase 1 has already
            // exited if any error was present. Run before the rewrite so
            // we see the original shape, not the lowered TaskGroup.
            for missed in detect_missed_gathers(&module) {
                let offset = missed.call_range.start().to_usize();
                let length = missed
                    .call_range
                    .end()
                    .to_usize()
                    .saturating_sub(offset)
                    .max(1);
                let advice = TycError::auto_gather_missed(
                    missed.missing_callee,
                    path.to_string_lossy().into_owned(),
                    &prep.python_source,
                    offset,
                    length,
                );
                eprintln!("{:?}", miette::Report::new_boxed(Box::new(advice)));
            }
            // Eligible callees = same-module `@gatherable` async defs,
            // PLUS `@gatherable` async defs imported from another project
            // module. The cross-module half resolves each `from M import
            // name` through the resolver's `import_info` (correct relative
            // / absolute / alias handling, shared with the type checker)
            // and consults the producer module's published
            // `gatherable_async_fns` set. So a run of
            // `await fetch_user(uid)` / `await fetch_posts(uid)` where both
            // are `@gatherable` in an imported `services` module folds into
            // an `asyncio.TaskGroup` just like a same-module run.
            let mut eligible = collect_gatherable_async_fn_names(&module);
            let (resolved, _) = tyc_resolve::resolve_module(
                path.to_string_lossy().into_owned(),
                &prep.python_source,
                &module,
            );
            if let Some(scope) = resolved.scopes.first() {
                for b in &scope.bindings {
                    let Some(info) = &b.import_info else { continue };
                    let Some(member) = &info.member else { continue };
                    if project_shapes
                        .get(&info.module)
                        .is_some_and(|s| s.gatherable_async_fns.contains(member))
                    {
                        eligible.insert(b.name.clone());
                    }
                }
            }
            let _stats = rewrite_auto_gather(&mut module, &eligible);
        }

        // Default-on concurrency nudge. Flag every remaining run of 2+
        // adjacent independent awaited calls inside an `async def` —
        // most commonly awaited method calls on imported clients
        // (`await client.get_user(id)` then `await client.get_posts(id)`),
        // which `auto-gather` never folds — so the user can wrap them in
        // an explicit `gather:` block and run them concurrently. Runs
        // already folded by `auto-gather` above are gone from the AST, so
        // they aren't re-flagged. Advice-only and printed straight through
        // miette (like the missed-gather advice above); never blocks a
        // build, and `[strictness] suggest-gather = false` silences it.
        if config.strictness.suggest_gather {
            // Same diagnostic construction the `check` command and the LSP
            // use, so the three surfaces never drift.
            for advice in tyc_analyse::gather_opportunity_diagnostics(
                &module,
                &path.to_string_lossy(),
                &prep.python_source,
            )
            .warnings()
            {
                eprintln!("{:?}", miette::Report::new_boxed(Box::new(advice.clone())));
            }
        }

        // Default-on performance-advice family (`tyc::perf_*` +
        // `tyc::lazy_import_opportunity`). Same detectors the `check` command
        // and the LSP run via `editor_lint_diagnostics`; advice-only, never
        // blocks a build, silenced by `[strictness] suggest-perf = false`.
        if config.strictness.suggest_perf {
            let perf_ctx = tyc_analyse::PerfLintContext {
                lazy_import_aliases: prep
                    .lazy_imports
                    .iter()
                    .map(|li| li.alias.clone())
                    .collect(),
                pub_names: prep.pub_names.clone(),
                has_pub_star: !prep.pub_star_lines.is_empty(),
            };
            for advice in tyc_analyse::perf_diagnostics(
                &module,
                &path.to_string_lossy(),
                &prep.python_source,
                &perf_ctx,
            )
            .warnings()
            {
                eprintln!("{:?}", miette::Report::new_boxed(Box::new(advice.clone())));
            }
        }

        // Default-on free-threading advice: `tyc::parallel_opportunity`
        // (a comprehension / accumulator loop that could be parallelised) and
        // `tyc::shared_mut_across_tasks` (a `go`-spawned callee that writes
        // shared mutable state). Both only fire when the project targets
        // free-threaded Python — the gate that keeps the example / stress
        // corpus quiet — and are silenced by `[strictness] suggest-parallel =
        // false`. Run *before* the auto-parallel / reduction rewrites so the
        // original comprehension / loop shapes are still present.
        if config.strictness.suggest_parallel && config.python.free_threaded {
            for advice in shared_mut_across_tasks_diagnostics(
                &module,
                &path.to_string_lossy(),
                &prep.python_source,
            )
            .warnings()
            {
                eprintln!("{:?}", miette::Report::new_boxed(Box::new(advice.clone())));
            }
            let pure_names: std::collections::HashSet<String> = purity_findings
                .iter()
                .filter(|f| f.violation.is_none())
                .map(|f| f.name.clone())
                .collect();
            for advice in parallel_opportunity_diagnostics(
                &module,
                &path.to_string_lossy(),
                &prep.python_source,
                &pure_names,
                config.strictness.parallel_min_size,
                config.strictness.auto_parallel.unwrap_or(false),
                config.strictness.auto_parallel_reductions,
            )
            .warnings()
            {
                eprintln!("{:?}", miette::Report::new_boxed(Box::new(advice.clone())));
            }
        }

        // Lower `extend BUILTIN:` blocks (e.g. `extend str:`) into module-
        // level free functions and rewrite call sites whose receiver has a
        // matching type annotation.  Receivers without a static annotation
        // fall through to native attribute access — matching Python's
        // existing semantics for missing methods.
        //
        // Cross-module extensions (#202): also merge extension registries
        // from imported modules so `title.slug()` in a consumer resolves
        // when `extend str: def slug(...)` was declared in a dependency.
        let (mut builtin_ext_registry, _ext_stats) = extract_builtin_extensions(&mut module);
        // Build cross-module extension registry scoped to modules that
        // the current file actually imports. This ensures the build path
        // agrees with the type-checker's import-based visibility and avoids
        // non-deterministic provider selection when multiple modules declare
        // `extend BUILTIN:` for the same type. (#202 review feedback)
        // `fn_name → source_module` reverse map for import injection.
        let mut cross_module_fns: HashMap<String, String> = HashMap::new();
        {
            use ruff_python_ast::Stmt;
            // Collect the set of module names this file imports.
            let mut imported_modules: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            for stmt in &module.body {
                match stmt {
                    Stmt::ImportFrom(i) => {
                        if let Some(m) = &i.module {
                            imported_modules.insert(m.id.to_string());
                        }
                    }
                    Stmt::Import(i) => {
                        for alias in &i.names {
                            imported_modules.insert(alias.name.id.to_string());
                        }
                    }
                    _ => {}
                }
            }
            for mod_name in &imported_modules {
                let Some(shapes) = project_shapes.get(mod_name) else {
                    continue;
                };
                for (cls_name, shape) in &shapes.class_shapes {
                    if let Some(builtin) = cls_name.strip_prefix("__typhon_builtin_ext_") {
                        let entry = builtin_ext_registry.entry(builtin.to_owned()).or_default();
                        for method_name in shape.methods.keys() {
                            let fn_name = format!("__typhon_ext_{builtin}__{method_name}");
                            entry.entry(method_name.clone()).or_insert_with(|| {
                                cross_module_fns.insert(fn_name.clone(), mod_name.clone());
                                fn_name
                            });
                        }
                    }
                }
            }
        }
        if !builtin_ext_registry.is_empty() {
            let (_rewrites, used_fns) =
                rewrite_builtin_extension_calls_tracking(&mut module, &builtin_ext_registry);
            // Inject `from <module> import <fn_name>` for cross-module
            // extension functions that were actually used. The injected
            // import uses the dotted module name; `from X import *` won't
            // carry `__`-prefixed names, so explicit import is required.
            let cross_module_used: Vec<(String, String)> = used_fns
                .iter()
                .filter_map(|fn_name| {
                    cross_module_fns
                        .get(fn_name)
                        .map(|mod_name| (mod_name.clone(), fn_name.clone()))
                })
                .collect();
            if !cross_module_used.is_empty() {
                inject_cross_module_ext_imports(&mut module, &cross_module_used);
            }
        }

        // Phase 4 loop parallelisation: rewrite `[f(x) for x in xs]` runs
        // whose callee is in the pure-function set into thread-pool maps.
        // Combine with `[python] free-threaded = true` for real parallelism;
        // on stock CPython the rewrite still happens but the GIL serialises
        // the workers (correctness preserved, no speedup).
        if config.strictness.auto_parallel.unwrap_or(false) {
            let pure_names: std::collections::HashSet<String> = purity_findings
                .iter()
                .filter(|f| f.violation.is_none())
                .map(|f| f.name.clone())
                .collect();
            let stats = rewrite_parallel_comprehensions(
                &mut module,
                &pure_names,
                config.strictness.parallel_min_size,
            );
            if stats.rewrites > 0 {
                needs_runtime = true;
            }
            // Integer accumulator-loop reductions, gated additionally on
            // `[strictness] auto-parallel-reductions`. Shares the pure-name
            // set with the comprehension rewrite; `total += EXPR` over a pure
            // body and a `mut total: int` accumulator becomes
            // `total += sum(map_pure(lambda x: EXPR, ITER))`. Integers only —
            // reordering float addition changes results, so floats are never
            // rewritten (they surface as `tyc::parallel_opportunity` advice).
            if config.strictness.auto_parallel_reductions {
                let rstats = rewrite_reduction_loops(
                    &mut module,
                    &pure_names,
                    config.strictness.parallel_min_size,
                );
                if rstats.rewrites > 0 {
                    needs_runtime = true;
                }
            }
        }

        let raw_class_line_starts = line_byte_starts(&prep.python_source, &prep.raw_class_lines);
        let frozen_class_line_starts =
            line_byte_starts(&prep.python_source, &prep.frozen_class_lines);
        let plain_class_line_starts =
            line_byte_starts(&prep.python_source, &prep.plain_class_lines);
        let desugar_output = desugar_module_with(
            &module,
            DesugarOptions {
                memoise_functions: memoise_targets,
                raw_class_line_starts,
                frozen_class_line_starts,
                plain_class_line_starts,
                skip_decoration_bases: config.emit.skip_decoration_bases.clone(),
                pub_names: prep.pub_names.clone(),
                model_extra: config.emit.model_extra.clone(),
                traceback_remap: config.emit.traceback_remap,
            },
        );
        if desugar_output.needs_typhon_runtime {
            needs_runtime = true;
        }
        // Build output must be valid Python, so emit with `let`/`mut`
        // suppressed at the AST level — a text-based strip would corrupt
        // string-literal contents that happen to start with those words.
        // Parse the configured Python minor version so the emitter can
        // lower PEP 695 syntax for targets < 3.12 (FINDINGS #47).
        // Anything we can't parse falls back to `0` (no lowering),
        // matching the previous default.
        let target_minor = parse_python_minor(&config.python.target);
        // Pass the preprocessed source so the printer can recover
        // stylistic choices the AST collapses — currently the bracket
        // style on sequence patterns (`case [a, b]:` vs `case (a, b):`).
        // The AST's TextRange offsets land in `prep.python_source`, not
        // the user's `.ty` text, so the printer needs the preprocessed
        // buffer. O27 / FINDINGS #111.
        let (mut python_src, emitted_line_offsets) = emit_python_with_source_for_target(
            &desugar_output.module,
            target_minor,
            Some(&prep.python_source),
        );
        // `line_offsets` must stay keyed to the buffer we actually write.
        // The formatting step below can reflow lines, so it re-keys this.
        let mut line_offsets = emitted_line_offsets;

        // Optionally normalise whitespace in the emitted Python (tabs → spaces,
        // trailing whitespace, final newline).  Full ruff-style reformatting
        // will replace this when the ruff vendor fork lands in Phase 3.
        if do_format {
            let path_str = path.to_string_lossy().into_owned();
            // Do NOT swallow the error. `format_source` fails when its own
            // parse step rejects the buffer, which means the emitter produced
            // something that is not valid Python — exactly the condition the
            // build must not exit 0 on.
            let result = format_source(&python_src, &path_str).map_err(|e| {
                miette!(
                    "internal error: formatting the Python emitted from '{}' failed: {e}\n\
                     This is a compiler bug — the emitted output is not valid Python. \
                     Please report it at https://github.com/CodeHalwell/Typhon/issues",
                    path.display()
                )
            })?;
            // Re-key the source-map offsets onto the formatted text.
            //
            // `format_source` runs two line-count-changing passes: the
            // in-process whitespace normaliser (which collapses runs of
            // 3+ blank lines and inserts PEP 8 blank lines before
            // top-level `def`/`class`) and, when `ruff` is on `$PATH`,
            // `ruff format` — which additionally wraps long calls and
            // signatures across several lines and joins short ones. Both
            // shift every subsequent entry of the emitter's table
            // relative to the file on disk, so `tyc trace`,
            // `tyc debug --break` and `[emit] traceback-remap` reported
            // lines that drifted further out the deeper into the file
            // the frame was. Diffing the two buffers recovers the
            // correspondence without the formatter having to report it.
            line_offsets =
                remap_line_offsets_through_format(&line_offsets, &python_src, &result.output);
            python_src = result.output;
        }

        // Post-emit parse gate.
        //
        // "Every `.ty` file emits valid `.py`" is the project's central
        // promise, and until now nothing checked it: a printer bug (missing
        // parens, a botched string escape, a preprocessor rewrite inside a
        // string literal) produced unparseable Python and the build still
        // exited 0. Re-parse the bytes we are about to write with the same
        // vendored parser the front end uses, and fail loudly if they do not
        // round-trip.
        //
        // This runs *before* the PEP 810 native-lazy-import rewrite below,
        // deliberately: that pass stamps `lazy import …` onto the buffer, and
        // no parser — vendored or upstream — accepts that syntax on a
        // pre-3.15 grammar. The rewrite only ever prefixes an existing,
        // already-validated `import` line, so gating ahead of it loses no
        // coverage.
        if let Err(err) = tyc_syntax::parse_module(&python_src) {
            return Err(miette!(
                "internal error: the Python emitted from '{}' does not parse: {err}\n\
                 This is a compiler bug — `tyc` must never write invalid Python. \
                 Please report it at https://github.com/CodeHalwell/Typhon/issues",
                path.display()
            ));
        }

        // PEP 810 native lazy-import lowering (3.15+ targets only). The
        // emitter has printed each recorded `lazy import ALIAS = MODULE` as a
        // plain `import MODULE as ALIAS`; prefix `lazy ` on the matching
        // module-level line so the artifact carries the native syntax. This
        // runs AFTER the formatter deliberately: the vendored ruff parser
        // (and an installed `ruff` on `$PATH`) can't parse `lazy import`, so
        // prefixing earlier would make `format_source` fail its parse step and
        // silently drop all formatting. Prefixing here keeps the plain-Python
        // buffer formattable and only stamps the keyword on at the very end.
        // The rewrite prepends to existing lines (no line-count change), so
        // the `.py.map` sidecar stays valid at line granularity.
        if native_lazy_imports && !prep.lazy_imports.is_empty() {
            python_src = prefix_native_lazy_imports(&python_src, &prep.lazy_imports);
        }

        let rel = path
            .strip_prefix(&src_dir)
            .map_err(|_| miette!("'{}' is outside the source directory", path.display()))?;
        let out_file = out_dir.join(rel).with_extension("py");

        if check_mode {
            println!("would write {}", display_relative(&out_file, &project_root));
            would_write_count += 1;
        } else {
            if let Some(parent) = out_file.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| miette!("cannot create '{}': {e}", parent.display()))?;
            }

            tyc_format::atomic_write(&out_file, python_src.as_bytes())
                .map_err(|e| miette!("cannot write '{}': {e}", out_file.display()))?;
        }

        // Emit a v2 `.py.map` sidecar under `<out>/.sourcemaps/<rel>.py.map`.
        //
        // Map files live in a dedicated `.sourcemaps/` subtree (mirroring
        // the emitted Python layout) so the build output stays tidy —
        // `ls build/` no longer interleaves every `foo.py` with its
        // sidecar. Consumers (`tyc trace`, `tyc debug`, `tyc ty`) resolve
        // maps via [`crate::commands::source_map::load_map_for`], which
        // checks `<out>/.sourcemaps/<rel>.py.map` first and falls back to
        // the legacy adjacent `<out>/<rel>.py.map` location for builds
        // emitted by older `tyc` versions.
        //
        // The `lines` array maps each Python output line (0-indexed) to a
        // 1-indexed line number in the preprocessed Typhon source.  For most
        // constructs the mapping is identity; sugar that emits multiple Python
        // lines from one Typhon line (e.g. `?`, `gather:`, `with`-chains)
        // correctly maps those lines back to the single originating line.
        let map_path = out_dir
            .join(".sourcemaps")
            .join(rel)
            .with_extension("py.map");
        let source_rel = escape_json_path(
            &path
                .strip_prefix(&src_dir)
                .unwrap_or(path)
                .display()
                .to_string(),
        );
        let map_body = build_source_map_v2(
            &source_rel,
            &prep.python_source,
            &line_offsets,
            &preprocessed_to_ty,
        );
        if check_mode {
            println!("would write {}", display_relative(&map_path, &project_root));
            would_write_count += 1;
            let _ = map_body;
        } else {
            if let Some(parent) = map_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| miette!("cannot create '{}': {e}", parent.display()))?;
            }
            tyc_format::atomic_write(&map_path, map_body.as_bytes())
                .map_err(|e| miette!("cannot write '{}': {e}", map_path.display()))?;
        }

        emitted += 1;
    }

    // Phase 5.4: copy stray `.py` files in `src/` to the build output
    // verbatim so emitted Python that does `from .helper import foo` finds
    // its hand-written sibling at runtime. Excludes `__pycache__/`,
    // `tests/`, `.venv/`, and hidden `.X/` directories (handled inside
    // `collect_py_files`).
    //
    // When `[project] out` is configured inside `[project] src` (e.g.
    // `out = "src/build"`), the scan would otherwise re-discover the
    // previously emitted Python and copy it into ever-nested
    // `build/build/...` paths on each run. Filter the output subtree
    // explicitly to keep the operation idempotent.
    let canonical_out_dir = out_dir.canonicalize().unwrap_or_else(|_| out_dir.clone());
    let py_files: Vec<_> = collect_py_files(&src_dir)?
        .into_iter()
        .filter(|path| {
            let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
            !canonical.starts_with(&canonical_out_dir)
        })
        .collect();
    let mut py_copied = 0usize;
    for path in &py_files {
        let rel = path
            .strip_prefix(&src_dir)
            .map_err(|_| miette!("'{}' is outside the source directory", path.display()))?;
        let dest = out_dir.join(rel);
        // A hand-written `src/X.py` lands on exactly the path the compiled
        // `src/X.ty` was just emitted to, so the copy silently replaced it:
        // the shipped program was not the program `tyc check` validated, and
        // the build still exited 0. The compiled output must win — a `.ty` is
        // the source of truth for its own module name — and the collision is
        // worth saying out loud, because a stale `.py` beside a `.ty` is
        // usually a leftover the author forgot to delete.
        //
        // Warn rather than fail: a *draft* `.ty` beside a working `.py` builds
        // today, and breaking that outright would reject a working project.
        if path.with_extension("ty").is_file() {
            let ty_rel = rel.with_extension("ty");
            eprintln!(
                "warning: '{}' shadows the Python compiled from '{}'. \
                 Keeping the compiled output; delete the .py file to silence this.",
                display_relative(path, &project_root),
                display_relative(&src_dir.join(&ty_rel), &project_root),
            );
            continue;
        }
        if check_mode {
            println!("would write {}", display_relative(&dest, &project_root));
            would_write_count += 1;
        } else {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| miette!("cannot create '{}': {e}", parent.display()))?;
            }
            std::fs::copy(path, &dest).map_err(|e| {
                miette!(
                    "cannot copy '{}' → '{}': {e}",
                    path.display(),
                    dest.display()
                )
            })?;
        }
        py_copied += 1;
    }
    if py_copied > 0 && !check_mode {
        println!("copied {} .py file(s)", py_copied);
    }

    // Phase 5.4 orphan-import warning: scan every `.ty` source for
    // `from .NAME import …` lines whose referenced `NAME.py` does NOT
    // exist anywhere under `src/`. Such relative imports compile fine
    // (the user's intent is clear) but the build won't copy the
    // referenced module — typically because `helper.py` lives in a
    // parent of `src/`. Best-effort textual scan; module-level
    // unknown-import diagnostics already cover the case where the file
    // is missing outright.
    let mut copied_py_module_names: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for path in &py_files {
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            copied_py_module_names.insert(stem.to_owned());
        }
    }
    for (path, source) in &sources {
        // Relative imports that escape the source root (`from ..x import …`
        // from a top-level module) crash at import — surface them here.
        if let Some(depth) = module_depth_below(path, &src_dir) {
            for (snippet, offset, length) in scan_overdeep_relative_imports(source, depth) {
                let warn = TycError::orphan_py_import(
                    snippet,
                    path.to_string_lossy().into_owned(),
                    source.clone(),
                    offset,
                    length,
                );
                eprintln!("{:?}", miette::Report::new_boxed(Box::new(warn)));
            }
        }
        for (module_name, snippet, offset, length) in scan_relative_py_imports(source) {
            if copied_py_module_names.contains(&module_name) {
                continue;
            }
            let mut found_orphan_parent = false;
            let mut walker: Option<&std::path::Path> = src_dir.parent();
            while let Some(dir) = walker {
                if dir.join(format!("{module_name}.py")).exists() {
                    found_orphan_parent = true;
                    break;
                }
                walker = dir.parent();
            }
            if found_orphan_parent {
                let warn = TycError::orphan_py_import(
                    snippet,
                    path.to_string_lossy().into_owned(),
                    source.clone(),
                    offset,
                    length,
                );
                eprintln!("{:?}", miette::Report::new_boxed(Box::new(warn)));
            }
        }
    }

    // Phase 3 stub emission: every `.dty` next to the project is compiled to a
    // PEP-561 `.pyi` so mypy / pyright / Pyrefly / ty can consume Typhon
    // authored libraries without an interop tax.  The `.dty` itself stays as
    // the authoritative document.
    let dty_files = collect_dty_files(&src_dir)?;
    let mut stubs_emitted = 0usize;
    for path in dty_files {
        let source = std::fs::read_to_string(&path)
            .map_err(|e| miette!("cannot read '{}': {e}", path.display()))?;
        // .dty files use the same syntax as .ty but typically contain only
        // declarations.  Run the preprocessor so `val`/`var`/`model` stripping
        // works, then desugar to plain Python so the printer can emit it.
        let expanded = expand_question_ops(&expand_inline_question_ops(
            &expand_compound_question_headers(&expand_pipes(&expand_with_chains(
                &expand_go_calls(&expand_gather_blocks(&expand_multiline_guards(
                    &expand_lazy_imports(&expand_typed_let_unpack(&source)),
                ))),
            ))),
        ));
        let prep = preprocess(&expanded);
        let module = tyc_syntax::parse_module(&prep.python_source)
            .map(|p| p.into_syntax())
            .map_err(|e| miette!("parse error in '{}': {e}", path.display()))?;
        let raw_class_line_starts = line_byte_starts(&prep.python_source, &prep.raw_class_lines);
        let frozen_class_line_starts =
            line_byte_starts(&prep.python_source, &prep.frozen_class_lines);
        let plain_class_line_starts =
            line_byte_starts(&prep.python_source, &prep.plain_class_lines);
        let desugar = desugar_module_with(
            &module,
            DesugarOptions {
                memoise_functions: Vec::new(),
                raw_class_line_starts,
                frozen_class_line_starts,
                plain_class_line_starts,
                skip_decoration_bases: config.emit.skip_decoration_bases.clone(),
                pub_names: prep.pub_names.clone(),
                model_extra: config.emit.model_extra.clone(),
                traceback_remap: config.emit.traceback_remap,
            },
        );
        let stub_text = emit_stub(&desugar.module);

        let rel = path
            .strip_prefix(&src_dir)
            .map_err(|_| miette!("'{}' is outside the source directory", path.display()))?;
        let out_file = out_dir.join(rel).with_extension("pyi");
        if check_mode {
            println!("would write {}", display_relative(&out_file, &project_root));
            would_write_count += 1;
            let _ = stub_text;
        } else {
            if let Some(parent) = out_file.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| miette!("cannot create '{}': {e}", parent.display()))?;
            }
            tyc_format::atomic_write(&out_file, stub_text.as_bytes())
                .map_err(|e| miette!("cannot write '{}': {e}", out_file.display()))?;
        }
        stubs_emitted += 1;
    }
    if stubs_emitted > 0 && !check_mode {
        println!("emitted {} stub(s) (.pyi)", stubs_emitted);
    }

    // Emit the typhon_runtime helper alongside the Python output when any
    // source file uses Ok, Err, Result, `go`, `lazy`, etc.  The helper is a
    // generated package the build owns; users do not need to install a
    // separate PyPI package.
    if needs_runtime {
        let runtime_dir = out_dir.join("typhon_runtime");
        // `parallel.py` is parameterised by the configured execution backend
        // (`[strictness] parallel-backend`); the rest are static.
        let parallel_py = typhon_runtime_parallel_py(
            &config.strictness.parallel_backend,
            config.strictness.parallel_min_size,
        );
        let files = [
            ("__init__.py", TYPHON_RUNTIME_INIT_PY),
            ("tasks.py", TYPHON_RUNTIME_TASKS_PY),
            ("lazy.py", TYPHON_RUNTIME_LAZY_PY),
            ("stdlib.py", TYPHON_RUNTIME_STDLIB_PY),
            ("result.py", TYPHON_RUNTIME_RESULT_PY),
            ("parallel.py", parallel_py.as_str()),
            ("freeze.py", TYPHON_RUNTIME_FREEZE_PY),
            ("cast.py", TYPHON_RUNTIME_CAST_PY),
            ("traceback.py", TYPHON_RUNTIME_TRACEBACK_PY),
        ];
        if check_mode {
            for (name, _body) in files {
                let path = runtime_dir.join(name);
                println!("would write {}", display_relative(&path, &project_root));
                would_write_count += 1;
            }
        } else {
            std::fs::create_dir_all(&out_dir)
                .map_err(|e| miette!("cannot create output dir '{}': {e}", out_dir.display()))?;
            std::fs::create_dir_all(&runtime_dir)
                .map_err(|e| miette!("cannot create '{}': {e}", runtime_dir.display()))?;
            for (name, body) in files {
                let path = runtime_dir.join(name);
                tyc_format::atomic_write(&path, body.as_bytes())
                    .map_err(|e| miette!("cannot write '{}': {e}", path.display()))?;
            }
            println!("wrote typhon_runtime/ → '{}'", runtime_dir.display());
        }
    }

    if check_mode {
        println!(
            "would write {} file(s) (no changes made)",
            would_write_count
        );
    } else {
        println!("built {} file(s) → '{}'", emitted, out_dir.display());
    }
    // R3 frontier: fail the build when `pub *` aggregated colliding
    // re-exports. The diagnostics were already printed to stderr in
    // the per-file loop; returning Err here ensures CI gates on them
    // instead of silently shipping the (order-dependent) shadow.
    if pub_star_collision_count > 0 {
        return Err(miette!(
            "{} `pub *` re-export collision(s) — \
             see the `tyc::pub_name_collision` advice above for details",
            pub_star_collision_count
        ));
    }

    // `[checker] external = "ty"`: run Astral's typeshed-backed checker over
    // the emitted Python as a second stage and re-attribute its diagnostics
    // back to the `.ty` source. Skipped in `--check` dry-run mode (no `.py`
    // was written to check). This is the only path that type-checks against
    // typeshed — covering C-extension libraries runtime introspection can't
    // see. See `docs/ty-integration.md`.
    if !check_mode && (args.with_ty || config.checker.external == "ty") {
        let reason = if args.with_ty {
            "--with-ty"
        } else {
            "[checker] external = \"ty\""
        };
        println!("running `ty` over emitted Python ({reason})…");
        crate::commands::ty::run_ty_check(
            &project_root,
            &out_dir,
            "ty",
            false,
            &config.checker.external_args,
        )?;
    }
    Ok(())
}

/// Inject `from <module> import <fn_name>` statements at the top of
/// `module` for cross-module builtin extension free functions that were
/// referenced during the rewrite pass. The injected imports are placed
/// after any existing imports at the top of the module body so they don't
/// disrupt `__future__` imports or docstrings. (#202)
fn inject_cross_module_ext_imports(
    module: &mut ruff_python_ast::ModModule,
    imports: &[(String, String)],
) {
    use ruff_python_ast::{name::Name, AtomicNodeIndex, Identifier, Stmt, StmtImportFrom};
    use ruff_text_size::TextRange;

    // Group by module so we emit one `from M import a, b, c` per module.
    let mut by_module: HashMap<String, Vec<String>> = HashMap::new();
    for (mod_name, fn_name) in imports {
        by_module
            .entry(mod_name.clone())
            .or_default()
            .push(fn_name.clone());
    }

    let mut injected: Vec<Stmt> = Vec::new();
    for (mod_name, fns) in &by_module {
        let aliases: Vec<ruff_python_ast::Alias> = fns
            .iter()
            .map(|f| ruff_python_ast::Alias {
                range: TextRange::default(),
                node_index: AtomicNodeIndex::NONE,
                name: Identifier {
                    range: TextRange::default(),
                    node_index: AtomicNodeIndex::NONE,
                    id: Name::new(f),
                },
                asname: None,
            })
            .collect();
        injected.push(Stmt::ImportFrom(StmtImportFrom {
            range: TextRange::default(),
            node_index: AtomicNodeIndex::NONE,
            module: Some(Identifier {
                range: TextRange::default(),
                node_index: AtomicNodeIndex::NONE,
                id: Name::new(mod_name),
            }),
            names: aliases,
            level: 0,
            is_lazy: false,
        }));
    }

    // Insert after the last existing import at the top of the module,
    // preserving the module's statement order. Only skip a leading
    // string-literal expression (module docstring) — other `Stmt::Expr`
    // are executable statements that must run after imports.
    let mut insert_pos = 0;
    // Skip optional leading docstring (bare string-literal expression).
    if let Some(Stmt::Expr(e)) = module.body.first() {
        if matches!(&*e.value, ruff_python_ast::Expr::StringLiteral(_)) {
            insert_pos = 1;
        }
    }
    // Skip past all contiguous imports.
    while insert_pos < module.body.len() {
        if matches!(
            &module.body[insert_pos],
            Stmt::Import(_) | Stmt::ImportFrom(_)
        ) {
            insert_pos += 1;
        } else {
            break;
        }
    }

    for (i, stmt) in injected.into_iter().enumerate() {
        module.body.insert(insert_pos + i, stmt);
    }
}

/// Render `path` as a project-root-relative display string when possible,
/// falling back to the absolute path. Used by the `--check` dry-run mode
/// to keep `would write …` lines readable.
fn display_relative(path: &std::path::Path, project_root: &std::path::Path) -> String {
    path.strip_prefix(project_root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string_lossy().into_owned())
}

/// Return `Some(suffix)` if `name` looks like a credential identifier.
/// The match is case-insensitive and ensures the token is bounded by the
/// start/end of the string or underscores.
/// Used by the secret-comptime lint.
fn secret_suffix(name: &str) -> Option<&'static str> {
    let upper = name.to_ascii_uppercase();
    // Keyword table shared with the `tyc::contains_secret_literal` lint —
    // see `tyc_analyse::SECRET_NAME_KEYWORDS` for the longest-first
    // ordering invariant both consumers rely on.
    for &candidate in tyc_analyse::SECRET_NAME_KEYWORDS {
        // We match if the substring is bounded by:
        // - start/end of string
        // - underscore (`_`)
        // - or if it's preceded/followed by a casing change (for camelCase)
        // e.g. "myTokenValue" -> "my" + "Token" + "Value" -> preceded by 'y' (lowercase),
        // followed by 'V' (uppercase).
        // Ensure "PASSPORT" does not match "PASS" by checking that the matched prefix/suffix
        // boundaries are actually word boundaries (i.e. we don't have uppercase letters directly next to uppercase letters in original, etc).
        // The simplest check for camelCase/PascalCase is:
        // start_ok: actual_idx == 0 OR previous char is `_` OR (previous char is lowercase AND current char is uppercase).
        // end_ok: actual_end == len OR next char is `_` OR (next char is uppercase AND last char of match was NOT uppercase).
        let mut start_idx = 0;
        while let Some(idx) = upper[start_idx..].find(candidate) {
            let actual_idx = start_idx + idx;

            let start_ok = actual_idx == 0
                || upper.as_bytes()[actual_idx - 1] == b'_'
                || name.as_bytes()[actual_idx - 1].is_ascii_digit()
                || (name.as_bytes()[actual_idx].is_ascii_uppercase()
                    && name.as_bytes()[actual_idx - 1].is_ascii_lowercase());

            let actual_end = actual_idx + candidate.len();
            let end_ok = actual_end == upper.len()
                || upper.as_bytes()[actual_end] == b'_'
                || name.as_bytes()[actual_end].is_ascii_digit()
                || (name.as_bytes()[actual_end].is_ascii_uppercase()
                    && !name.as_bytes()[actual_end - 1].is_ascii_uppercase())
                || (name.as_bytes()[actual_end].is_ascii_uppercase()
                    && actual_end + 1 < name.len()
                    && name.as_bytes()[actual_end + 1].is_ascii_lowercase());

            if start_ok && end_ok {
                return Some(candidate);
            }
            start_idx = actual_idx + 1;
        }
    }
    None
}

/// Scan `source` for `from .NAME import …` lines and return
/// `(NAME, snippet)` pairs. Single-dot relative imports only — this
/// lint targets sibling-file imports, not parent-package `from ..pkg
/// import …` references. Lightweight textual scan; works even before
/// the file successfully parses. Returns `(name, snippet, offset, length)`
/// where `offset`/`length` describe the byte-range of the trimmed import
/// line in `source` — used to build a `TycError::OrphanPyImport`
/// diagnostic with a source span pointing at the actual import.
fn scan_relative_py_imports(source: &str) -> Vec<(String, String, usize, usize)> {
    let mut out = Vec::new();
    let mut line_start = 0usize;
    for line in source.split_inclusive('\n') {
        let line_len = line.len();
        let trimmed_start = line_start
            + line
                .as_bytes()
                .iter()
                .take_while(|&&b| b == b' ' || b == b'\t')
                .count();
        let trimmed = line.trim_start();
        let rest = match trimmed.strip_prefix("from .") {
            Some(r) => r,
            None => {
                line_start += line_len;
                continue;
            }
        };
        if rest.starts_with('.') {
            line_start += line_len;
            continue;
        }
        let mut name = String::new();
        for ch in rest.chars() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                name.push(ch);
            } else {
                break;
            }
        }
        if name.is_empty() {
            line_start += line_len;
            continue;
        }
        let after = &rest[name.len()..];
        if !after.trim_start().starts_with("import") {
            line_start += line_len;
            continue;
        }
        let snippet = trimmed.trim_end().to_owned();
        out.push((name, snippet.clone(), trimmed_start, snippet.len()));
        line_start += line_len;
    }
    out
}

/// A module's package depth below `src_dir` — the number of package
/// directories between `src_dir` and the file. `src/main.ty` → 0 (top-level
/// package), `src/a/mod.ty` → 1, etc. `None` when the path can't be expressed
/// relative to `src_dir` (so the over-deep-import check is skipped rather than
/// risking a false positive).
pub(crate) fn module_depth_below(
    path: &std::path::Path,
    src_dir: &std::path::Path,
) -> Option<usize> {
    let rel = path.strip_prefix(src_dir).ok()?;
    Some(rel.components().count().saturating_sub(1))
}

/// Scan for relative imports whose dot-level escapes the source root. A module
/// at package depth `D` below the source root can ascend at most `D` levels: a
/// top-level module (`depth` 0) cannot use *any* relative import (run as
/// `python build/main.py` it has no parent package, so even `from . import …`
/// raises `ImportError: attempted relative import with no known parent
/// package`), a depth-1 module may use `from . import` (level 1) but not
/// `from .. import` (level 2), and so on. A `from <dots>x` with
/// `dots > depth` reaches above the package root and crashes the emitted
/// Python at import, so flag it at check/build time. Returns
/// `(snippet, offset, length)` per offending line.
pub(crate) fn scan_overdeep_relative_imports(
    source: &str,
    module_depth: usize,
) -> Vec<(String, usize, usize)> {
    let max_level = module_depth;
    let mut out = Vec::new();
    let mut line_start = 0usize;
    for line in source.split_inclusive('\n') {
        let line_len = line.len();
        let leading_ws = line
            .as_bytes()
            .iter()
            .take_while(|&&b| b == b' ' || b == b'\t')
            .count();
        let trimmed_start = line_start + leading_ws;
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("from ") {
            let rest = rest.trim_start();
            let dots = rest.chars().take_while(|&c| c == '.').count();
            // Must be an actual `import` statement, not e.g. a string.
            if dots > max_level && trimmed.contains("import") {
                let snippet = trimmed.trim_end().to_owned();
                let len = snippet.len();
                out.push((snippet, trimmed_start, len));
            }
        }
        line_start += line_len;
    }
    out
}

/// Find the env-var key in a `comptime let NAME ... = env("KEY"...)`
/// declaration by scanning `source` for the binding's line. Returns the
/// first quoted string immediately following `env(` on a line that mentions
/// `NAME`. Returns `None` when the binding's RHS doesn't use `env(...)` —
/// e.g. `comptime let X = 42`, where the secret lint shouldn't fire.
fn find_env_key_for_comptime_binding(source: &str, binding_name: &str) -> Option<String> {
    let needle = "comptime ";
    for line in source.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with(needle) {
            continue;
        }
        // Match either `comptime let NAME` or `comptime mut NAME` or
        // bare `comptime NAME` (legacy). Cheaply check the binding name
        // appears before the `=`.
        let lhs = trimmed.split('=').next().unwrap_or("");
        let lhs_has_name = lhs.split_whitespace().any(|tok| {
            tok.trim_end_matches(':')
                .trim_end_matches(',')
                .eq(binding_name)
        });
        if !lhs_has_name {
            continue;
        }
        // Locate the first `env(` after the `=` and lift the first quoted
        // string out of its argument list.
        let after_eq = match trimmed.split_once('=') {
            Some((_, r)) => r,
            None => continue,
        };
        let env_idx = after_eq.find("env(")?;
        let after_open = &after_eq[env_idx + "env(".len()..];
        // Strip optional whitespace and grab the leading `"..."` or `'...'`.
        let after_ws = after_open.trim_start();
        let quote = after_ws.chars().next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        let rest = &after_ws[1..];
        let end = rest.find(quote)?;
        return Some(rest[..end].to_owned());
    }
    None
}

/// Byte offset of the start of the 0-based `line_idx` line in `source`.
/// Used by the `pub *` aggregation pass to anchor `tyc::pub_star_*`
/// diagnostic spans at the original marker line. Returns `source.len()`
/// when the line index is out of range so the diagnostic still renders.
fn line_offset(source: &str, line_idx: usize) -> usize {
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

/// Replace the 0-based `line_idx` line of `source` with `replacement`
/// (the newline terminator is preserved). Used by the `pub *`
/// aggregation pass to inject `from .sibling import …` blocks at the
/// original `pub *` marker so the rest of the pipeline emits the
/// imports as normal.
fn replace_line(source: &str, line_idx: usize, replacement: &str) -> String {
    let mut out = String::with_capacity(source.len() + replacement.len());
    for (current, line) in source.split_inclusive('\n').enumerate() {
        if current == line_idx {
            out.push_str(replacement);
            if line.ends_with('\n') {
                out.push('\n');
            }
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Compute the effective public surface of a package directory.
/// Walks `__init__.ty`'s top-level `pub` names, then — if that
/// `__init__.ty` itself contains a `pub *` marker — recurses into
/// every direct sub-package and direct .ty sibling at one level
/// deeper, mirroring the aggregation the build-time `pub *` rewrite
/// does in place. The result is the set of names a hypothetical
/// `from <pkg> import *` would receive.
///
/// `visited` carries every package directory that's already been
/// expanded along the current chain, breaking cycles that arise from
/// pathological repository layouts (e.g. a sub-package whose
/// `__init__.ty` re-points at its own grandparent). The walk is
/// loaded-source-only — it never touches the filesystem, so an
/// unloaded sub-tree is silently skipped.
fn effective_package_surface(
    pkg_dir: &std::path::Path,
    sources: &[(PathBuf, String)],
    visited: &mut std::collections::HashSet<PathBuf>,
) -> Vec<String> {
    let pkg_owned = pkg_dir.to_path_buf();
    if !visited.insert(pkg_owned.clone()) {
        return Vec::new();
    }
    let init_path = pkg_dir.join("__init__.ty");
    let init_source = match sources.iter().find(|(p, _)| p == &init_path) {
        Some((_, s)) => s,
        None => return Vec::new(),
    };
    let mut out: Vec<String> = extract_top_level_pub_names(init_source);
    if !source_has_pub_star_marker(init_source) {
        return out;
    }
    // Aggregate one level deeper: direct .ty siblings of this
    // __init__.ty + direct sub-packages.
    let mut sub_packages_seen: std::collections::HashSet<PathBuf> =
        std::collections::HashSet::new();
    for (sib_path, sib_source) in sources {
        let sib_parent = match sib_path.parent() {
            Some(p) => p,
            None => continue,
        };
        if sib_parent == pkg_dir {
            let is_init = sib_path
                .file_name()
                .and_then(|f| f.to_str())
                .map(|s| s == "__init__.ty")
                .unwrap_or(false);
            if is_init {
                continue;
            }
            for name in extract_top_level_pub_names(sib_source) {
                if !out.contains(&name) {
                    out.push(name);
                }
            }
            continue;
        }
        if sib_parent.parent() == Some(pkg_dir)
            && sub_packages_seen.insert(sib_parent.to_path_buf())
        {
            let sub_init = sib_parent.join("__init__.ty");
            if !sources.iter().any(|(p, _)| p == &sub_init) {
                continue;
            }
            for name in effective_package_surface(sib_parent, sources, visited) {
                if !out.contains(&name) {
                    out.push(name);
                }
            }
        }
    }
    out
}

/// Fast textual extractor for top-level (zero-indent) `pub <kw> NAME`
/// declarations in a Typhon source file. Used by the `pub *`
/// aggregation pre-pass so the orchestrator can know each sibling
/// module's public surface without paying for a full preprocess of
/// every sibling. Mirrors the logic
/// [`tyc_syntax::preprocess::pub_decl_name`] applies, but operates
/// directly on the raw text.
/// True when `source` contains a `pub *` marker at module level (zero
/// indent) that the preprocessor would recognise. Mirrors
/// `tyc_syntax::preprocess::is_pub_star_line`'s acceptance rules so
/// invalid forms like `pub * from foo` and occurrences inside
/// triple-quoted strings / comments don't falsely trigger transitive
/// aggregation. Used by `effective_package_surface` to gate recursion
/// into sub-packages.
/// B28: collect `tyc::pub_name_collision` diagnostics for every
/// `__init__.ty` whose `pub *` re-export would conflict with itself
/// across sibling modules. Shared between `tyc build` (which fails the
/// build on any collision) and `tyc check` (which surfaces them in CI
/// before they reach build). Also returns `tyc::pub_star_outside_init`
/// advice diagnostics for `pub *` markers in non-`__init__` files.
///
/// `sources` is the full file tree (path + raw `.ty` text) the caller
/// has loaded. The returned vector is in source-order per file.
pub(crate) fn detect_pub_star_diagnostics(
    sources: &[(PathBuf, String)],
) -> (
    Vec<tyc_diagnostics::TycError>,
    Vec<tyc_diagnostics::TycError>,
) {
    use tyc_syntax::preprocess::{
        expand_gather_blocks, expand_go_calls, expand_inline_question_ops, expand_lazy_imports,
        expand_multiline_guards, expand_pipes, expand_question_ops, expand_typed_let_unpack,
        expand_with_chains, preprocess,
    };
    let mut errors: Vec<tyc_diagnostics::TycError> = Vec::new();
    let mut advice: Vec<tyc_diagnostics::TycError> = Vec::new();

    for (path, source) in sources {
        let expanded = expand_question_ops(&expand_inline_question_ops(
            &expand_compound_question_headers(&expand_pipes(&expand_with_chains(
                &expand_go_calls(&expand_gather_blocks(&expand_multiline_guards(
                    &expand_lazy_imports(&expand_typed_let_unpack(source)),
                ))),
            ))),
        ));
        let prep = preprocess(&expanded);
        if prep.pub_star_lines.is_empty() {
            continue;
        }
        let is_init = path
            .file_name()
            .and_then(|f| f.to_str())
            .map(|s| s == "__init__.ty")
            .unwrap_or(false);

        if !is_init {
            for &line_idx in &prep.pub_star_lines {
                let offset = line_offset(&expanded, line_idx);
                advice.push(tyc_diagnostics::TycError::pub_star_outside_init(
                    path.to_string_lossy().into_owned(),
                    expanded.clone(),
                    offset,
                    5,
                ));
            }
            continue;
        }

        // __init__.ty branch: walk siblings + sub-packages and detect
        // duplicate names. Mirror of the inline aggregation in
        // `tyc build`. The collision detection itself ignores the
        // import-block synthesis (build does that for emit; check
        // only cares about correctness).
        let Some(parent_dir) = path.parent() else {
            continue;
        };
        let mut sibling_pubs: Vec<(String, Vec<String>)> = Vec::new();
        for (sib_path, sib_source) in sources {
            let sib_parent = match sib_path.parent() {
                Some(p) => p,
                None => continue,
            };
            if sib_parent != parent_dir {
                continue;
            }
            let stem = sib_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if stem == "__init__" {
                continue;
            }
            let names = extract_top_level_pub_names(sib_source);
            if !names.is_empty() {
                sibling_pubs.push((stem.to_owned(), names));
            }
        }
        let mut sub_packages_seen: std::collections::HashSet<PathBuf> =
            std::collections::HashSet::new();
        let mut visited: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        visited.insert(parent_dir.to_path_buf());
        for (sib_path, _) in sources {
            let sib_parent = match sib_path.parent() {
                Some(p) => p,
                None => continue,
            };
            if sib_parent.parent() == Some(parent_dir)
                && sub_packages_seen.insert(sib_parent.to_path_buf())
            {
                let sub_init = sib_parent.join("__init__.ty");
                if !sources.iter().any(|(p, _)| p == &sub_init) {
                    continue;
                }
                let sub_name = match sib_parent.file_name().and_then(|s| s.to_str()) {
                    Some(s) => s.to_owned(),
                    None => continue,
                };
                let names = effective_package_surface(sib_parent, sources, &mut visited);
                if !names.is_empty() {
                    sibling_pubs.push((sub_name, names));
                }
            }
        }
        sibling_pubs.sort_by(|a, b| a.0.cmp(&b.0));

        let mut name_origin: std::collections::HashMap<String, String> = prep
            .pub_names
            .iter()
            .map(|n| (n.clone(), "<package>".to_owned()))
            .collect();
        let marker_line = *prep.pub_star_lines.first().unwrap_or(&0);
        let marker_offset = line_offset(&expanded, marker_line);
        for (sibling, names) in &sibling_pubs {
            for name in names {
                if let Some(prev) = name_origin.get(name).cloned() {
                    errors.push(tyc_diagnostics::TycError::pub_name_collision(
                        name.clone(),
                        prev,
                        sibling.clone(),
                        path.to_string_lossy().into_owned(),
                        expanded.clone(),
                        marker_offset,
                        5,
                    ));
                } else {
                    name_origin.insert(name.clone(), sibling.clone());
                }
            }
        }
    }
    (errors, advice)
}

fn source_has_pub_star_marker(source: &str) -> bool {
    let mut in_triple_string = false;
    for line in source.split_inclusive('\n') {
        let triple_count = line.matches("\"\"\"").count() + line.matches("'''").count();
        if in_triple_string {
            if triple_count % 2 == 1 {
                in_triple_string = false;
            }
            continue;
        }
        if triple_count % 2 == 1 {
            in_triple_string = true;
            continue;
        }
        let indent_len = line
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(line.len());
        if indent_len != 0 {
            continue;
        }
        let rest = &line[indent_len..];
        let Some(after) = rest.strip_prefix("pub *") else {
            continue;
        };
        let trimmed = after.trim_start_matches([' ', '\t']);
        let body = trimmed.trim_end_matches(['\n', '\r']);
        if body.is_empty() || body.starts_with('#') {
            return true;
        }
    }
    false
}

fn extract_top_level_pub_names(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_triple_string = false;
    for line in source.split_inclusive('\n') {
        // Skip the content of triple-quoted strings (docstrings,
        // multi-line literals) so a `"""... pub class ... """` block
        // doesn't masquerade as a real declaration.
        let triple_count = line.matches("\"\"\"").count() + line.matches("'''").count();
        if in_triple_string {
            if triple_count % 2 == 1 {
                in_triple_string = false;
            }
            continue;
        }
        if triple_count % 2 == 1 {
            in_triple_string = true;
            continue;
        }
        // Only zero-indent lines start a top-level declaration.
        let indent_len = line
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(line.len());
        if indent_len != 0 {
            continue;
        }
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let Some(after_pub) = trimmed.strip_prefix("pub ") else {
            continue;
        };
        if let Some(name) = tyc_syntax::preprocess::pub_decl_name(after_pub) {
            out.push(name);
        }
    }
    out
}

/// Derive the dotted Python module name that the runtime profiler
/// would record for a `.ty` source file. `src/main.ty` becomes `main`;
/// `src/pkg/sub/helpers.ty` becomes `pkg.sub.helpers`. The matcher in
/// `pgo_memoise_targets` keys profile lookups on this string so the
/// build doesn't confuse same-named functions in different modules.
fn python_module_name_from_path(path: &std::path::Path, src_dir: &std::path::Path) -> String {
    let rel = path.strip_prefix(src_dir).unwrap_or(path);
    let stem = rel.with_extension("");
    let mut parts: Vec<String> = Vec::new();
    for component in stem.components() {
        if let std::path::Component::Normal(s) = component {
            if let Some(name) = s.to_str() {
                parts.push(name.to_owned());
            }
        }
    }
    parts.join(".")
}

/// Return `true` if any source file declares a `model X:` class. Used to
/// auto-inject `pydantic` into `[dependencies]` before the pyproject.toml
/// merge, since the emitter will produce `from pydantic import …`
/// statements for those classes.
fn sources_use_model_keyword(sources: &[(PathBuf, String)]) -> bool {
    sources.iter().any(|(_, text)| source_uses_model(text))
}

fn source_uses_model(text: &str) -> bool {
    // Track triple-quoted string state so a docstring or SQL string
    // containing a line starting with `model ` doesn't falsely flag
    // the source as needing pydantic.
    let mut in_triple: Option<&'static str> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();

        // Update triple-quote tracking before deciding whether this line
        // is code: a `"""…"""` opener that doesn't close on the same line
        // puts us into multi-line-string mode for subsequent lines.
        let (next_state, line_starts_in_string) = update_triple_quote_state(line, in_triple);
        let was_in_string = in_triple.is_some();
        in_triple = next_state;
        if was_in_string || line_starts_in_string {
            continue;
        }

        if !trimmed.starts_with("model ") {
            continue;
        }
        // A line starting with `model ` only signals a model class when
        // the next non-space char is an identifier character (so we
        // reject `model "X":` and similar). Comments are already
        // filtered out because `trim_start` leaves `#` in place.
        let after = &trimmed["model ".len()..];
        let first = after.chars().next();
        if matches!(first, Some(c) if c.is_ascii_alphabetic() || c == '_') {
            return true;
        }
    }
    false
}

/// Walk `line` and update the triple-quoted-string state. Returns the
/// state at the end of the line plus a flag indicating whether the
/// line *started* inside a string (in which case the caller treats it
/// as pure string content). Handles both `"""` and `'''` delimiters
/// and counts opener/closer pairs per line so a single-line
/// `"""...""".format(...)` doesn't leave the scanner stuck.
fn update_triple_quote_state(
    line: &str,
    mut state: Option<&'static str>,
) -> (Option<&'static str>, bool) {
    let started_in_string = state.is_some();
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i + 2 < bytes.len() + 1 {
        let three = bytes.get(i..i + 3);
        match (state, three) {
            (None, Some(b"\"\"\"")) => {
                state = Some("\"\"\"");
                i += 3;
                continue;
            }
            (None, Some(b"'''")) => {
                state = Some("'''");
                i += 3;
                continue;
            }
            (Some("\"\"\""), Some(b"\"\"\"")) => {
                state = None;
                i += 3;
                continue;
            }
            (Some("'''"), Some(b"'''")) => {
                state = None;
                i += 3;
                continue;
            }
            _ => {}
        }
        // When outside a string, stop scanning at a `#` comment start so
        // a `# """foo"""` comment doesn't toggle state.
        if state.is_none() && bytes[i] == b'#' {
            break;
        }
        i += 1;
    }
    (state, started_in_string)
}

/// Parse a `[python] target = "3.X"` string into its minor version.
/// Returns `0` for unparseable / unrecognised values (telling the
/// emitter to skip PEP 695 lowering and keep the previous behaviour).
/// Tolerates `"3.13"`, `"3.13t"` (free-threaded), `"3.10"`, etc.
fn parse_python_minor(target: &str) -> u8 {
    let rest = target.strip_prefix("3.").unwrap_or(target);
    // Strip any trailing alphabetical suffix ("t" for free-threaded).
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u8>().unwrap_or(0)
}

/// Rewrite the emitted Python so each recorded `lazy import ALIAS = MODULE`
/// carries native PEP 810 syntax on a Python 3.15+ target.
///
/// On the 3.15+ build path the main preprocessor lowered every
/// `lazy import ALIAS = MODULE` to a plain `import MODULE as ALIAS`
/// statement and recorded the `(alias, module)` pair in
/// `prep.lazy_imports`. This pass finds the matching emitted line and
/// prepends the `lazy ` keyword, producing `lazy import MODULE as ALIAS`
/// — the native deferred-import form CPython 3.15 understands directly.
///
/// Matching rules (deliberately conservative):
///   * Only **module-level** lines (no leading indentation) are eligible —
///     a `lazy import` is only recognised at column 0 upstream, so an
///     indented `import numpy as np` inside a function is never a lazy one.
///   * The line's code must equal exactly `import MODULE as ALIAS`. For the
///     `alias == module` case the emitter prints `import numpy as numpy`
///     (it never elides a redundant `as`); a bare `import MODULE` is also
///     accepted there as a defensive fallback in case a formatter collapses
///     the redundant alias.
///   * Each recorded lazy import claims the **first** unclaimed matching
///     line, and at most one line. A recorded import with no matching line
///     is skipped, leaving that line untouched rather than corrupting the
///     file.
///
/// Prepending never changes the line count, so the `.py.map` sidecar (built
/// from the pre-format emit offsets) stays valid at line granularity.
fn prefix_native_lazy_imports(src: &str, lazy_imports: &[LazyImport]) -> String {
    let mut claimed = vec![false; lazy_imports.len()];
    let mut out = String::with_capacity(src.len() + lazy_imports.len() * 5);
    for line in src.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        let trailing = &line[content.len()..];
        let mut prefixed = false;
        // Module level only: reject any line that starts with whitespace.
        if !content.starts_with([' ', '\t']) {
            for (i, li) in lazy_imports.iter().enumerate() {
                if claimed[i] {
                    continue;
                }
                let want_as = format!("import {} as {}", li.module, li.alias);
                let matches = content == want_as
                    || (li.alias == li.module && content == format!("import {}", li.module));
                if matches {
                    out.push_str("lazy ");
                    out.push_str(content);
                    out.push_str(trailing);
                    claimed[i] = true;
                    prefixed = true;
                    break;
                }
            }
        }
        if !prefixed {
            out.push_str(line);
        }
    }
    out
}

/// Minimal JSON string escape for paths used in the `.py.map` body.  Only
/// backslashes and double quotes need escaping; the rest of ASCII passes
/// through unchanged.  Non-ASCII bytes (e.g. UTF-8 multi-byte sequences) are
/// passed through verbatim — modern JSON parsers accept them.
fn escape_json_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            other => out.push(other),
        }
    }
    out
}

/// Run one `&str -> (String, line map)` stage of the sugar-expansion chain,
/// folding its table into the one accumulated so far.
///
/// `stage` is `(text, text line -> .ty line)`. After the call it describes the
/// pass's output, still keyed back to the original `.ty` file.
fn chain_step(stage: &mut (String, Vec<usize>), pass: fn(&str) -> (String, Vec<usize>)) {
    let (text, step_map) = pass(&stage.0);
    stage.1 = compose_line_maps(&step_map, &stage.1);
    stage.0 = text;
}

/// Recursion budget for [`align_range`].
///
/// Each level consumes at least one anchor line from both buffers, so the
/// depth is bounded by the file length anyway; the cap only exists so a
/// pathological input degrades to the proportional fill instead of
/// recursing thousands of frames deep on the CLI's worker stack.
const ALIGN_MAX_DEPTH: usize = 32;

/// Align the lines of a formatted buffer back onto the buffer it was
/// formatted from.
///
/// `tyc-emit` records one source offset per line of the Python it prints,
/// but with `[emit] format = true` that buffer is then handed to the
/// whitespace normaliser and (when `ruff` is on `$PATH`) to `ruff format`,
/// either of which can insert, delete, join or wrap lines. The offsets
/// therefore describe a file that is *not* the one written to disk, and
/// every entry after the first reflow is shifted — the further into the
/// file, the worse. Consumers (`tyc trace`, `tyc debug --break`,
/// `[emit] traceback-remap`, `ty` re-attribution) all index the sidecar by
/// the line number CPython reports for the file on disk, so the table has
/// to be keyed to the formatted text.
///
/// The returned vector has one entry per line of `after`, holding the
/// 0-based index of the `before` line it came from. Composing it with the
/// emitter's table re-keys the whole map onto the formatted buffer.
///
/// The alignment is a patience diff: exact common prefix / suffix first
/// (the overwhelmingly common case — most files come back byte-identical,
/// which this resolves in one linear scan), then lines that occur exactly
/// once in both remaining ranges are taken as anchors via a longest
/// increasing subsequence, and the gaps between anchors recurse. A gap
/// with no anchors is distributed proportionally, which is exactly right
/// for the shape that motivates this: one long statement wrapped across
/// several output lines has a one-line `before` gap, so every wrapped
/// line lands on it.
fn align_formatted_lines(before: &[&str], after: &[&str]) -> Vec<usize> {
    let mut out = vec![0usize; after.len()];
    if before.is_empty() || after.is_empty() {
        return out;
    }
    align_range(before, after, 0, before.len(), 0, after.len(), 0, &mut out);
    out
}

/// Align `after[a0..a1)` onto `before[b0..b1)`, writing into `out`.
#[allow(clippy::too_many_arguments)]
fn align_range(
    before: &[&str],
    after: &[&str],
    mut b0: usize,
    mut b1: usize,
    mut a0: usize,
    mut a1: usize,
    depth: usize,
    out: &mut [usize],
) {
    while b0 < b1 && a0 < a1 && before[b0] == after[a0] {
        out[a0] = b0;
        b0 += 1;
        a0 += 1;
    }
    while b1 > b0 && a1 > a0 && before[b1 - 1] == after[a1 - 1] {
        b1 -= 1;
        a1 -= 1;
        out[a1] = b1;
    }
    if a0 >= a1 {
        // Nothing left on the formatted side: any surviving `before`
        // lines were deleted by the formatter and simply have no entry.
        return;
    }
    if b0 >= b1 {
        // Pure insertion (a blank line the formatter added, say).
        // Attribute it to the nearest surviving neighbour, preferring the
        // line above so an inserted blank before a `def` keeps naming the
        // statement it follows rather than jumping ahead.
        let anchor = if b0 > 0 { b0 - 1 } else { 0 };
        for slot in out.iter_mut().take(a1).skip(a0) {
            *slot = anchor;
        }
        return;
    }
    if depth < ALIGN_MAX_DEPTH {
        let anchors = patience_anchors(before, after, b0, b1, a0, a1);
        if !anchors.is_empty() {
            let mut prev_b = b0;
            let mut prev_a = a0;
            for (bi, ai) in anchors {
                align_range(before, after, prev_b, bi, prev_a, ai, depth + 1, out);
                out[ai] = bi;
                prev_b = bi + 1;
                prev_a = ai + 1;
            }
            align_range(before, after, prev_b, b1, prev_a, a1, depth + 1, out);
            return;
        }
    }
    if align_gap_by_content(before, after, b0, b1, a0, a1, out) {
        return;
    }
    let bn = b1 - b0;
    let an = a1 - a0;
    for (k, slot) in out.iter_mut().take(a1).skip(a0).enumerate() {
        *slot = b0 + (k * bn / an).min(bn - 1);
    }
}

/// Bytes the formatter may insert or delete without that counting as a
/// divergence: the brackets it adds when wrapping an expression across
/// lines, the ones it drops when a redundant pair becomes unnecessary,
/// and the "magic trailing comma" it appends to a wrapped argument list.
fn is_formatter_punctuation(b: u8) -> bool {
    matches!(b, b'(' | b')' | b',')
}

/// Align an anchor-less gap by walking both sides' non-whitespace bytes
/// in lockstep.
///
/// Two adjacent statements that a formatter both wraps produce one gap
/// with no line-level anchor in it, and splitting such a gap
/// proportionally lands lines on the wrong statement. Reflowing does not
/// reorder or rewrite code, though — it only moves whitespace and adds or
/// removes the punctuation above — so the two byte streams still match
/// almost exactly. Each formatted line takes the `before` line that owns
/// the first byte it matched; a line that matched nothing (a blank, or a
/// bracket the walk skipped as an insertion) inherits its predecessor,
/// which is what a wrapped continuation line wants.
///
/// Returns `false`, leaving `out` untouched, on any divergence reflowing
/// cannot explain (a quote-style or numeric-literal normalisation, say).
/// The caller then falls back to the proportional split rather than
/// trusting a walk that has lost sync.
#[allow(clippy::too_many_arguments)]
fn align_gap_by_content(
    before: &[&str],
    after: &[&str],
    b0: usize,
    b1: usize,
    a0: usize,
    a1: usize,
    out: &mut [usize],
) -> bool {
    fn stream(lines: &[&str], lo: usize, hi: usize) -> Vec<(u8, usize)> {
        let mut v = Vec::new();
        for (i, line) in lines.iter().enumerate().take(hi).skip(lo) {
            v.extend(
                line.bytes()
                    .filter(|b| !b.is_ascii_whitespace())
                    .map(|b| (b, i)),
            );
        }
        v
    }
    let bs = stream(before, b0, b1);
    let as_ = stream(after, a0, a1);
    if bs.is_empty() || as_.is_empty() {
        return false;
    }

    let mut first: Vec<Option<usize>> = vec![None; a1 - a0];
    let mut i = 0usize;
    let mut j = 0usize;
    while i < as_.len() {
        let (ac, aline) = as_[i];
        if j < bs.len() && bs[j].0 == ac {
            first[aline - a0].get_or_insert(bs[j].1);
            i += 1;
            j += 1;
            continue;
        }
        if j >= bs.len() {
            // A tail the formatter added — the closing bracket of a
            // wrapped call on a line of its own. It belongs to the last
            // statement of the gap.
            first[aline - a0].get_or_insert(b1 - 1);
            i += 1;
            continue;
        }
        if is_formatter_punctuation(ac) {
            i += 1;
            continue;
        }
        if is_formatter_punctuation(bs[j].0) {
            j += 1;
            continue;
        }
        return false;
    }

    let mut last = b0;
    for (k, slot) in out.iter_mut().take(a1).skip(a0).enumerate() {
        if let Some(b) = first[k] {
            last = b;
        }
        *slot = last;
    }
    true
}

/// Anchor pairs for a patience diff over `before[b0..b1)` / `after[a0..a1)`.
///
/// A candidate is a line that appears exactly once in each range and is
/// equal on both sides; the returned pairs are the longest increasing
/// subsequence of those candidates, so they are strictly increasing in
/// both indices and can be used to split the ranges. Blank lines are
/// never unique in a real file, so they self-exclude — which is what we
/// want, since matching them carries no information.
fn patience_anchors(
    before: &[&str],
    after: &[&str],
    b0: usize,
    b1: usize,
    a0: usize,
    a1: usize,
) -> Vec<(usize, usize)> {
    // `(count, first index)` per distinct line, for each side.
    let mut b_seen: HashMap<&str, (u32, usize)> = HashMap::new();
    for (i, line) in before.iter().enumerate().take(b1).skip(b0) {
        b_seen
            .entry(line)
            .and_modify(|e| e.0 += 1)
            .or_insert((1, i));
    }
    let mut a_seen: HashMap<&str, (u32, usize)> = HashMap::new();
    for (i, line) in after.iter().enumerate().take(a1).skip(a0) {
        a_seen
            .entry(line)
            .and_modify(|e| e.0 += 1)
            .or_insert((1, i));
    }

    // Candidates ordered by `before` index; each carries its `after` index.
    let mut candidates: Vec<(usize, usize)> = b_seen
        .iter()
        .filter(|(_, &(count, _))| count == 1)
        .filter_map(|(line, &(_, bi))| match a_seen.get(line) {
            Some(&(1, ai)) => Some((bi, ai)),
            _ => None,
        })
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    candidates.sort_unstable();

    // Longest increasing subsequence over the `after` indices (patience
    // sorting): `tails[k]` is the candidate index ending the best length-
    // `k+1` chain found so far, `prev` threads the chain backwards.
    let mut tails: Vec<usize> = Vec::new();
    let mut tail_values: Vec<usize> = Vec::new();
    let mut prev: Vec<Option<usize>> = vec![None; candidates.len()];
    for (idx, &(_, ai)) in candidates.iter().enumerate() {
        let pos = tail_values.partition_point(|&v| v < ai);
        prev[idx] = if pos > 0 { Some(tails[pos - 1]) } else { None };
        if pos == tails.len() {
            tails.push(idx);
            tail_values.push(ai);
        } else {
            tails[pos] = idx;
            tail_values[pos] = ai;
        }
    }

    let mut chain = Vec::with_capacity(tails.len());
    let mut cursor = tails.last().copied();
    while let Some(idx) = cursor {
        chain.push(candidates[idx]);
        cursor = prev[idx];
    }
    chain.reverse();
    chain
}

/// Re-key `line_offsets` (one entry per line of the *emitted* Python) onto
/// the formatted buffer that is actually written to disk.
///
/// Returns one entry per line of `formatted`. See
/// [`align_formatted_lines`] for why this is needed.
fn remap_line_offsets_through_format(
    line_offsets: &[usize],
    unformatted: &str,
    formatted: &str,
) -> Vec<usize> {
    if unformatted == formatted {
        return line_offsets.to_vec();
    }
    let before: Vec<&str> = unformatted.lines().collect();
    let after: Vec<&str> = formatted.lines().collect();
    let alignment = align_formatted_lines(&before, &after);
    // `line_offsets` has one entry per emitted line; a formatted line that
    // aligns past its end can only come from trailing synthesised text, so
    // clamp to the last known offset rather than dropping the entry.
    let fallback = line_offsets.last().copied().unwrap_or(0);
    alignment
        .iter()
        .map(|&i| line_offsets.get(i).copied().unwrap_or(fallback))
        .collect()
}

/// Build a v2 `.py.map` JSON body with a full `lines` table.
///
/// `line_offsets[i]` is the byte offset in `preprocessed` that was "active"
/// when output line `i` (0-indexed) was emitted.  Each offset is converted to
/// a 0-indexed line number *of the preprocessed buffer*, and then — this is
/// the step that makes the sidecar mean what it claims — through
/// `preprocessed_to_ty` to the 0-based `.ty` line it actually came from,
/// emitted 1-indexed.  Synthesised lines (offset 0) land on the file's first
/// line, matching the identity fallback.
///
/// Passing an empty `preprocessed_to_ty` selects the identity mapping, which
/// is only correct for a buffer no pass changed the line count of; real builds
/// always pass the folded chain table.
fn build_source_map_v2(
    source_rel: &str,
    preprocessed: &str,
    line_offsets: &[usize],
    preprocessed_to_ty: &[usize],
) -> String {
    // ⚡ Bolt optimization: Precompute newline offsets to avoid O(N^2) behavior.
    // Instead of rescanning the string for every offset, we find all newlines
    // once O(N) and binary search them for each offset O(log N).
    let newline_offsets: Vec<usize> = preprocessed
        .as_bytes()
        .iter()
        .enumerate()
        .filter_map(|(i, &b)| if b == b'\n' { Some(i) } else { None })
        .collect();

    let lines: Vec<u32> = line_offsets
        .iter()
        .map(|&offset| {
            let clamped = offset.min(preprocessed.len());
            let count = newline_offsets.partition_point(|&nl_offset| nl_offset < clamped);
            let ty_line = if preprocessed_to_ty.is_empty() {
                count
            } else {
                // Clamp rather than drop: a token offset past the last mapped
                // line can only come from trailing synthesised text, which
                // belongs to the last real source line.
                preprocessed_to_ty[count.min(preprocessed_to_ty.len() - 1)]
            };
            ty_line as u32 + 1
        })
        .collect();

    let mut out = String::with_capacity(64 + lines.len() * 4);
    out.push_str("{\"version\":2,\"source\":\"");
    out.push_str(source_rel);
    out.push_str("\",\"line_strategy\":\"table\",\"lines\":[");

    let mut buf = itoa::Buffer::new();
    for (i, &n) in lines.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(buf.format(n));
    }

    out.push_str("]}\n");
    out
}

// ── Comptime literal substitution ─────────────────────────────────────────────
//
// `substitute_comptime_literals` was moved to `tyc-analyse` so the VM
// (`tyc run`) can share the same comptime-inlining pass that the
// `tyc build` command uses. Both consumers now import the public
// `substitute_comptime_literals` re-export at the top of this file.
// Transformation: `PORT: int = int(env("PORT", "8080"))` →
// `PORT: int = 8080`.

/// Generated `typhon_runtime/__init__.py` — exposes `Ok`/`Err`/`Result` plus
/// the `tasks` and `lazy` submodules at the package root.
///
/// Emitted whenever a `.ty` source references any of `Ok`/`Err`/`Result`, the
/// `go` keyword, or the `lazy` keyword.  No separate PyPI package is required
/// to deploy a Typhon project.
const TYPHON_RUNTIME_INIT_PY: &str = "\
# generated by tyc — do not edit
from __future__ import annotations

from dataclasses import dataclass
from typing import Generic, TypeVar

from . import lazy, parallel, result, stdlib, tasks  # re-exported for `typhon_runtime.<sub>.…`

_T = TypeVar(\"_T\")
_E = TypeVar(\"_E\")


@dataclass(slots=True, frozen=True)
class Ok(Generic[_T]):
    value: _T

    def map(self, f):
        return Ok(f(self.value))

    def map_err(self, f):
        # Ok is unchanged by map_err — the error transform is irrelevant.
        return self

    def and_then(self, f):
        return f(self.value)

    def or_else(self, f):
        return self

    def unwrap(self):
        return self.value

    def expect(self, msg):
        return self.value

    def unwrap_or(self, default):
        return self.value

    def unwrap_or_else(self, f):
        return self.value

    def ok(self):
        return self.value

    def err(self):
        return None

    def is_ok(self):
        return True

    def is_err(self):
        return False


@dataclass(slots=True, frozen=True)
class Err(Generic[_E]):
    error: _E

    def map(self, f):
        # Err is unchanged by map — the value transform is irrelevant.
        return self

    def map_err(self, f):
        return Err(f(self.error))

    def and_then(self, f):
        return self

    def or_else(self, f):
        return f(self.error)

    def unwrap(self):
        raise RuntimeError(f\"called unwrap() on Err: {self.error!r}\")

    def expect(self, msg):
        raise RuntimeError(f\"{msg}: {self.error!r}\")

    def unwrap_or(self, default):
        return default

    def unwrap_or_else(self, f):
        return f(self.error)

    def ok(self):
        return None

    def err(self):
        return self.error

    def is_ok(self):
        return False

    def is_err(self):
        return True


# Use `typing.Union` rather than PEP 695 `type Result[T, E] = …` so the
# generated runtime loads under Python 3.10 / 3.11 / 3.12 as well as the
# 3.13+ default. The runtime never inspects the alias's generic
# parameters — `isinstance(x, Err)` and the dataclass shape are what the
# rest of the runtime relies on — so dropping the parameters here is
# harmless. Static type checkers still see `Result` as a union of `Ok`
# and `Err`.
from typing import TypeAlias, Union
Result: TypeAlias = Union[Ok, Err]


def try_result(thunk, on_err=None):
    \"\"\"Run ``thunk()`` and return ``Ok(result)``; on any exception return
    ``Err(on_err(exc))`` (or ``Err(exc)`` when no mapper is given).

    Backs Typhon's ``try_result`` exception->Result bridging combinator, so a
    library boundary reads as ``let r = try_result(lambda: json.load(f), lambda
    e: str(e))`` instead of a hand-written ``try/except`` that returns
    ``Ok``/``Err``.\"\"\"
    try:
        return Ok(thunk())
    except Exception as exc:  # noqa: BLE001 - bridging an untyped boundary is intentionally broad
        return Err(exc if on_err is None else on_err(exc))


__all__ = [\"Ok\", \"Err\", \"Result\", \"try_result\", \"tasks\", \"lazy\", \"stdlib\", \"result\", \"parallel\"]
";

/// Generated `typhon_runtime/tasks.py` — strong-reference task registry.
///
/// `spawn(coro)` schedules `coro` on the running event loop and keeps a strong
/// reference to the resulting `asyncio.Task` until the task completes, so the
/// event loop's weak-ref behaviour does not garbage-collect the task
/// mid-flight.
const TYPHON_RUNTIME_TASKS_PY: &str = "\
# generated by tyc — do not edit
\"\"\"Strong-reference task registry for the `go` keyword.\"\"\"
from __future__ import annotations

import asyncio
from typing import Awaitable, TypeVar

_T = TypeVar(\"_T\")

_BACKGROUND: set[asyncio.Task] = set()


def spawn(coro: Awaitable[_T]) -> asyncio.Task[_T]:
    \"\"\"Schedule *coro* and hold a strong reference until it finishes.\"\"\"
    task = asyncio.create_task(coro)
    _BACKGROUND.add(task)
    task.add_done_callback(_BACKGROUND.discard)
    return task
";

/// Generated `typhon_runtime/lazy.py` — lazy-import and lazy-val helpers.
///
/// `lazy_import(name)` returns a proxy that imports the module on first
/// attribute access. `lazy_let(factory)` returns a proxy that materialises the
/// underlying value on first attribute access (and forwards attribute lookups,
/// item subscripts, and calls transparently afterwards).
const TYPHON_RUNTIME_LAZY_PY: &str = "\
# generated by tyc — do not edit
\"\"\"Helpers backing the `lazy import` and `lazy let` Typhon keywords.\"\"\"
from __future__ import annotations

import importlib
import importlib.util
import threading
from types import ModuleType
from typing import Callable, TypeVar

_T = TypeVar(\"_T\")


class _LazyModule:
    \"\"\"Attribute-proxy that imports its underlying module on first access.\"\"\"

    __slots__ = (\"_name\", \"_module\")

    def __init__(self, name: str) -> None:
        object.__setattr__(self, \"_name\", name)
        object.__setattr__(self, \"_module\", None)

    def _load(self) -> ModuleType:
        module = object.__getattribute__(self, \"_module\")
        if module is None:
            module = importlib.import_module(object.__getattribute__(self, \"_name\"))
            object.__setattr__(self, \"_module\", module)
        return module

    def __getattr__(self, attr: str) -> object:
        return getattr(self._load(), attr)

    def __dir__(self) -> list[str]:
        return dir(self._load())

    def __repr__(self) -> str:
        module = object.__getattribute__(self, \"_module\")
        if module is None:
            name = object.__getattribute__(self, \"_name\")
            return f\"<lazy module {name!r}: unloaded>\"
        return repr(module)


def lazy_import(name: str) -> _LazyModule:
    \"\"\"Return a module proxy that defers loading until first attribute access.

    The return value is a `_LazyModule` proxy, not a real `ModuleType` —
    `isinstance(lazy_import(...), ModuleType)` is `False`. Attribute access
    transparently forwards to the loaded module via `__getattr__`.\"\"\"
    return _LazyModule(name)


class _LazyValue:
    \"\"\"Proxy that materialises an underlying value on first use.\"\"\"

    __slots__ = (\"_factory\", \"_value\", \"_lock\")

    def __init__(self, factory: Callable[[], _T]) -> None:
        object.__setattr__(self, \"_factory\", factory)
        object.__setattr__(self, \"_value\", _UNSET)
        object.__setattr__(self, \"_lock\", threading.Lock())

    def _materialise(self) -> object:
        value = object.__getattribute__(self, \"_value\")
        if value is _UNSET:
            with object.__getattribute__(self, \"_lock\"):
                value = object.__getattribute__(self, \"_value\")
                if value is _UNSET:
                    factory = object.__getattribute__(self, \"_factory\")
                    value = factory()
                    object.__setattr__(self, \"_value\", value)
        return value

    def __getattr__(self, name: str) -> object:
        return getattr(self._materialise(), name)

    def __call__(self, *args: object, **kwargs: object) -> object:
        return self._materialise()(*args, **kwargs)

    def __getitem__(self, key: object) -> object:
        return self._materialise()[key]

    def __iter__(self) -> object:
        return iter(self._materialise())

    def __repr__(self) -> str:
        value = object.__getattribute__(self, \"_value\")
        if value is _UNSET:
            return \"<lazy: unmaterialised>\"
        return repr(value)

    def __str__(self) -> str:
        # Materialise on `str(...)` / `print(...)` so the value is
        # readable instead of the proxy's `<lazy: unmaterialised>`
        # debug repr. FINDINGS #105.
        return str(self._materialise())

    def __bool__(self) -> bool:
        return bool(self._materialise())

    def __eq__(self, other: object) -> bool:
        return self._materialise() == other

    def __hash__(self) -> int:
        return hash(self._materialise())

    def __len__(self) -> int:
        return len(self._materialise())

    # Forward arithmetic, comparison, bitwise, unary, conversion, index and
    # membership operators to the materialised value so a `lazy let` of a
    # primitive (int/float/str/bytes/…) is transparent under every operator,
    # not just attribute access. Without these, `VALUE + 1`, `VALUE > 10`,
    # `range(VALUE)`, `NAME + \" world\"` etc. raised TypeError against the
    # proxy even though the underlying value supports them.
    def __add__(self, other: object) -> object:
        return self._materialise() + other

    def __radd__(self, other: object) -> object:
        return other + self._materialise()

    def __sub__(self, other: object) -> object:
        return self._materialise() - other

    def __rsub__(self, other: object) -> object:
        return other - self._materialise()

    def __mul__(self, other: object) -> object:
        return self._materialise() * other

    def __rmul__(self, other: object) -> object:
        return other * self._materialise()

    def __truediv__(self, other: object) -> object:
        return self._materialise() / other

    def __rtruediv__(self, other: object) -> object:
        return other / self._materialise()

    def __floordiv__(self, other: object) -> object:
        return self._materialise() // other

    def __rfloordiv__(self, other: object) -> object:
        return other // self._materialise()

    def __mod__(self, other: object) -> object:
        return self._materialise() % other

    def __rmod__(self, other: object) -> object:
        return other % self._materialise()

    def __pow__(self, other: object) -> object:
        return self._materialise() ** other

    def __rpow__(self, other: object) -> object:
        return other ** self._materialise()

    def __and__(self, other: object) -> object:
        return self._materialise() & other

    def __rand__(self, other: object) -> object:
        return other & self._materialise()

    def __or__(self, other: object) -> object:
        return self._materialise() | other

    def __ror__(self, other: object) -> object:
        return other | self._materialise()

    def __xor__(self, other: object) -> object:
        return self._materialise() ^ other

    def __rxor__(self, other: object) -> object:
        return other ^ self._materialise()

    def __lshift__(self, other: object) -> object:
        return self._materialise() << other

    def __rlshift__(self, other: object) -> object:
        return other << self._materialise()

    def __rshift__(self, other: object) -> object:
        return self._materialise() >> other

    def __rrshift__(self, other: object) -> object:
        return other >> self._materialise()

    def __lt__(self, other: object) -> bool:
        return self._materialise() < other

    def __le__(self, other: object) -> bool:
        return self._materialise() <= other

    def __gt__(self, other: object) -> bool:
        return self._materialise() > other

    def __ge__(self, other: object) -> bool:
        return self._materialise() >= other

    def __neg__(self) -> object:
        return -self._materialise()

    def __pos__(self) -> object:
        return +self._materialise()

    def __abs__(self) -> object:
        return abs(self._materialise())

    def __invert__(self) -> object:
        return ~self._materialise()

    def __int__(self) -> int:
        return int(self._materialise())

    def __float__(self) -> float:
        return float(self._materialise())

    def __index__(self) -> int:
        return self._materialise().__index__()

    def __contains__(self, item: object) -> bool:
        return item in self._materialise()


_UNSET = object()


def lazy_let(factory: Callable[[], _T]) -> _T:
    \"\"\"Return a proxy that calls *factory* on first attribute access.\"\"\"
    # Cast for the type checker; behaviour-wise the proxy forwards everything.
    return _LazyValue(factory)  # type: ignore[return-value]
";

/// Generated `typhon_runtime/stdlib.py` — Typhon's small native stdlib.
///
/// These helpers fill gaps in CPython's stdlib that Typhon code hits often:
/// safe parsing into `Result`, sequence/iterator combinators, and string
/// utilities. They are pure Python so they ship as part of the generated
/// package — no third-party install required.
const TYPHON_RUNTIME_STDLIB_PY: &str = "\
# generated by tyc — do not edit
\"\"\"Typhon's native standard library.

A small, deliberately conservative set of helpers that are awkward to
express with CPython's stdlib alone but show up constantly in Typhon code
(safe parsing, iterator folds, string utilities, retry loops).
\"\"\"
from __future__ import annotations

import time
from typing import Callable, Iterable, Iterator, TypeVar

_T = TypeVar(\"_T\")
_U = TypeVar(\"_U\")


def _ok_err() -> tuple[type, type]:
    # Resolved lazily so this module is safe to import before the package's
    # __init__.py has finished defining Ok / Err.
    from . import Err, Ok  # noqa: PLC0415

    return Ok, Err

__all__ = [
    \"parse_int\",
    \"parse_float\",
    \"chunked\",
    \"flatten\",
    \"unique\",
    \"group_by\",
    \"strip_prefix\",
    \"strip_suffix\",
    \"split_once\",
    \"retry\",
]


def parse_int(s: str, base: int = 10) -> object:
    \"\"\"Parse *s* as an `int`. Returns `Err(reason)` instead of raising.\"\"\"
    Ok, Err = _ok_err()
    try:
        return Ok(int(s, base))
    except (TypeError, ValueError) as exc:
        return Err(str(exc))


def parse_float(s: str) -> object:
    \"\"\"Parse *s* as a `float`. Returns `Err(reason)` instead of raising.\"\"\"
    Ok, Err = _ok_err()
    try:
        return Ok(float(s))
    except (TypeError, ValueError) as exc:
        return Err(str(exc))


def chunked(items: Iterable[_T], size: int) -> Iterator[list[_T]]:
    \"\"\"Yield successive *size*-sized chunks from *items*.\"\"\"
    if size <= 0:
        raise ValueError(\"chunk size must be positive\")
    batch: list[_T] = []
    for item in items:
        batch.append(item)
        if len(batch) == size:
            yield batch
            batch = []
    if batch:
        yield batch


def flatten(items: Iterable[Iterable[_T]]) -> Iterator[_T]:
    \"\"\"Yield every element from every iterable in *items*.\"\"\"
    for inner in items:
        for x in inner:
            yield x


def unique(items: Iterable[_T]) -> Iterator[_T]:
    \"\"\"Yield each element from *items* the first time it is seen.

    Elements must be hashable; this uses a `set` for O(1) membership.
    For unhashable inputs build your own dedup keyed on `id()` or a
    user-provided key function.
    \"\"\"
    seen: set[_T] = set()
    for item in items:
        if item in seen:
            continue
        seen.add(item)
        yield item


def group_by(items: Iterable[_T], key: Callable[[_T], _U]) -> dict[_U, list[_T]]:
    \"\"\"Group *items* into a dict keyed by `key(item)`. Order preserved.

    The result of `key(item)` must be hashable (the runtime's `Ok` / `Err`
    are `frozen=True` so they qualify).
    \"\"\"
    groups: dict[_U, list[_T]] = {}
    for item in items:
        groups.setdefault(key(item), []).append(item)
    return groups


def strip_prefix(s: str, prefix: str) -> str | None:
    \"\"\"Return *s* with *prefix* removed, or `None` if no match.\"\"\"
    if s.startswith(prefix):
        return s[len(prefix):]
    return None


def strip_suffix(s: str, suffix: str) -> str | None:
    \"\"\"Return *s* with *suffix* removed, or `None` if no match.

    `strip_suffix(s, \"\")` returns `s` (mirrors `strip_prefix`'s
    behaviour for an empty separator).
    \"\"\"
    if not s.endswith(suffix):
        return None
    return s[: len(s) - len(suffix)]


def split_once(s: str, sep: str) -> tuple[str, str] | None:
    \"\"\"Split *s* on the first occurrence of *sep*. None if *sep* missing.\"\"\"
    idx = s.find(sep)
    if idx < 0:
        return None
    return (s[:idx], s[idx + len(sep):])


def retry(
    op: Callable[[], _T],
    *,
    attempts: int = 3,
    backoff: float = 0.1,
    factor: float = 2.0,
) -> object:
    \"\"\"Call *op* up to *attempts* times with exponential backoff.\"\"\"
    if attempts <= 0:
        raise ValueError(\"attempts must be positive\")
    Ok, Err = _ok_err()
    delay = backoff
    last: Exception | None = None
    for i in range(attempts):
        try:
            return Ok(op())
        except Exception as exc:  # noqa: BLE001 — KeyboardInterrupt/SystemExit propagate
            last = exc
            if i == attempts - 1:
                break
            time.sleep(delay)
            delay *= factor
    assert last is not None
    return Err(last)
";

/// Generated `typhon_runtime/result.py` — combinators for `Result[T, E]`.
///
/// The `Ok` / `Err` constructors themselves live in `__init__.py`; this
/// module adds the methods Typhon users reach for when chaining `Result`
/// values without falling into nested `match` ladders.
const TYPHON_RUNTIME_RESULT_PY: &str = "\
# generated by tyc — do not edit
\"\"\"Combinators for `Result[T, E]` — map / and_then / unwrap helpers.

Several names here (`map`, `and_then`, `unwrap`) collide with builtins
or common identifiers if star-imported.  Prefer qualified access via
`typhon_runtime.result.map(r, f)` over `from typhon_runtime.result
import map`, which would shadow `builtins.map` in the caller's scope.
\"\"\"
from __future__ import annotations

from typing import Callable, TypeVar

_T = TypeVar(\"_T\")
_U = TypeVar(\"_U\")
_E = TypeVar(\"_E\")
_F = TypeVar(\"_F\")

# Imported lazily inside each helper to avoid a circular import with the
# package `__init__.py`, which itself imports this submodule.

__all__ = [
    \"is_ok\",
    \"is_err\",
    \"map\",
    \"map_err\",
    \"and_then\",
    \"or_else\",
    \"unwrap\",
    \"unwrap_or\",
    \"unwrap_or_else\",
]


def _types() -> tuple[type, type]:
    from . import Err as _Err, Ok as _Ok  # local to dodge import cycle

    return _Ok, _Err


def is_ok(r: object) -> bool:
    Ok, _Err = _types()
    return isinstance(r, Ok)


def is_err(r: object) -> bool:
    _Ok, Err = _types()
    return isinstance(r, Err)


def map(r: object, f: Callable[[_T], _U]) -> object:
    \"\"\"Transform the `Ok` payload with *f*; pass `Err` through untouched.\"\"\"
    Ok, _Err = _types()
    if isinstance(r, Ok):
        return Ok(f(r.value))
    return r


def map_err(r: object, f: Callable[[_E], _F]) -> object:
    \"\"\"Transform the `Err` payload with *f*; pass `Ok` through untouched.\"\"\"
    _Ok, Err = _types()
    if isinstance(r, Err):
        return Err(f(r.error))
    return r


def and_then(r: object, f: Callable[[_T], object]) -> object:
    \"\"\"Chain another `Result`-returning op on the `Ok` payload.\"\"\"
    Ok, _Err = _types()
    if isinstance(r, Ok):
        return f(r.value)
    return r


def or_else(r: object, f: Callable[[_E], object]) -> object:
    \"\"\"Recover from `Err` by calling *f* with the error and returning its result.\"\"\"
    _Ok, Err = _types()
    if isinstance(r, Err):
        return f(r.error)
    return r


def unwrap(r: object) -> object:
    \"\"\"Return the `Ok` payload or raise `ValueError` on `Err`.\"\"\"
    Ok, Err = _types()
    if isinstance(r, Ok):
        return r.value
    if isinstance(r, Err):
        raise ValueError(f\"unwrap on Err: {r.error!r}\")
    raise TypeError(f\"not a Result: {r!r}\")


def unwrap_or(r: object, default: _T) -> _T:
    Ok, _Err = _types()
    if isinstance(r, Ok):
        return r.value
    return default


def unwrap_or_else(r: object, f: Callable[[_E], _T]) -> _T:
    Ok, Err = _types()
    if isinstance(r, Ok):
        return r.value
    if isinstance(r, Err):
        return f(r.error)
    raise TypeError(f\"not a Result: {r!r}\")
";

/// Generated `typhon_runtime/parallel.py` — thread-pool helpers backing
/// the `[strictness] auto-parallel` rewrite.
///
/// `map_pure(fn, iterable, *, max_workers=None)` returns a `list` of
/// `fn(x)` results in the iterable's original order.  Uses
/// `concurrent.futures.ThreadPoolExecutor.map`, which under free-threaded
/// CPython (3.13t+) escapes the GIL entirely and yields linear scaling
/// for CPU-bound `fn`s.  On stock CPython the workers still serialise on
/// the GIL — correctness is preserved, only the speedup is lost.
/// Generated `typhon_runtime/freeze.py` — recursive deep-freeze helper.
///
/// `deep_freeze(value)` walks the value and replaces every mutable
/// container in the tree with an immutable equivalent (`list → tuple`,
/// `dict → MappingProxyType`, `set → frozenset`). Leaves primitives,
/// strings, bytes, and frozen dataclass instances alone. Raises
/// `TypeError` on values that cannot be frozen (e.g. open file
/// handles, sockets, generators) so the user finds out at build /
/// startup time rather than after a successful freeze that silently
/// shared a mutable reference.
const TYPHON_RUNTIME_FREEZE_PY: &str = "\
# generated by tyc — do not edit
\"\"\"Deep-freeze helper backing Typhon's `freeze let` keyword.\"\"\"
from __future__ import annotations

from types import MappingProxyType
from typing import Any

_FROZEN_PRIMITIVES = (
    int,
    float,
    bool,
    str,
    bytes,
    type(None),
    complex,
    type(Ellipsis),
    type(NotImplemented),
)

# Containers that are already immutable at runtime — pass through unchanged.
_FROZEN_CONTAINERS = (tuple, frozenset, MappingProxyType, range, bytes)


def deep_freeze(value: Any) -> Any:
    \"\"\"Return a deeply-immutable version of *value*.

    Recursively replaces `list → tuple`, `dict → MappingProxyType`,
    `set → frozenset`. Primitives, strings, and already-immutable
    containers are returned as-is. Raises `TypeError` when *value*
    holds something with no clean immutable equivalent so users find
    out at startup rather than via a subtle aliasing bug later.

    Cycles (a list that contains itself, mutual references between
    dicts) are rejected with `TypeError` rather than recursing to a
    stack overflow.
    \"\"\"
    return _deep_freeze(value, set())


def _deep_freeze(value: Any, seen: set[int]) -> Any:
    if isinstance(value, _FROZEN_PRIMITIVES):
        return value
    # Cycle guard: only mutable containers (list, dict, set) and the
    # immutable containers we descend into can form cycles, so track
    # `id(value)` for those branches. Primitives are skipped above.
    value_id = id(value)
    if value_id in seen:
        raise TypeError(
            f\"deep_freeze cannot freeze {type(value).__name__}; \"
            \"the value contains a reference cycle\"
        )
    if isinstance(value, _FROZEN_CONTAINERS):
        # Even though the container itself is immutable, its contents may
        # not be — descend into tuples and frozensets so a tuple of lists
        # really is deeply immutable after one call.
        if isinstance(value, tuple):
            seen.add(value_id)
            try:
                return tuple(_deep_freeze(v, seen) for v in value)
            finally:
                seen.discard(value_id)
        if isinstance(value, frozenset):
            seen.add(value_id)
            try:
                return frozenset(_deep_freeze(v, seen) for v in value)
            finally:
                seen.discard(value_id)
        return value
    if isinstance(value, list):
        seen.add(value_id)
        try:
            return tuple(_deep_freeze(v, seen) for v in value)
        finally:
            seen.discard(value_id)
    if isinstance(value, dict):
        seen.add(value_id)
        try:
            return MappingProxyType({k: _deep_freeze(v, seen) for k, v in value.items()})
        finally:
            seen.discard(value_id)
    if isinstance(value, set):
        seen.add(value_id)
        try:
            return frozenset(_deep_freeze(v, seen) for v in value)
        finally:
            seen.discard(value_id)
    # `@dataclass(frozen=True)` instances already block field reassignment
    # at the dataclass level. We can't deep-freeze their nested fields
    # without rebuilding the instance, which would defeat identity. Accept
    # them as-is and document that field-level deep freezing is the
    # author's responsibility.
    import dataclasses
    if dataclasses.is_dataclass(value) and getattr(type(value), \"__dataclass_params__\", None) is not None:
        params = type(value).__dataclass_params__
        if getattr(params, \"frozen\", False):
            return value
    raise TypeError(
        f\"deep_freeze cannot freeze {type(value).__name__}; \"
        f\"types without an immutable equivalent (open handles, generators, \"
        f\"non-frozen dataclasses, …) must not appear in a `freeze let` value\"
    )
";

/// Generated `typhon_runtime/cast.py` — the runtime guard backing Typhon's
/// `as!` checked boundary cast.
///
/// `EXPR as! TYPE` lowers to `checked_cast(EXPR, TYPE)`. Unlike a static-only
/// re-assertion (which trusts the boundary value blindly), this verifies the
/// value's shape against `TYPE` at runtime and raises `TypeError` on a
/// mismatch, so an `as!` cast is genuinely *checked* rather than an unsound
/// `Any`-style escape hatch. Structural element checks recurse through
/// parameterised containers (`list[int]`, `dict[str, int]`, `tuple[int, ...]`,
/// unions, `Optional`). `Any` / `object` targets and shapes the helper can't
/// model fall back to acceptance, so the cast can only reject values it can
/// prove wrong.
const TYPHON_RUNTIME_CAST_PY: &str = "\
# generated by tyc — do not edit
\"\"\"Runtime guard backing Typhon's `as!` checked boundary cast.\"\"\"
from __future__ import annotations

import types
import typing
from typing import Any


def checked_cast(value: Any, tp: Any) -> Any:
    \"\"\"Return *value* if it structurally matches *tp*, else raise TypeError.

    Backs `EXPR as! TYPE`. The static type is `TYPE` (the checker handles
    that); this enforces the same shape at runtime so the boundary cast is
    sound rather than a blind assertion.
    \"\"\"
    if _matches(value, tp):
        return value
    raise TypeError(
        f\"as! cast failed: value of type {type(value).__name__} \"
        f\"does not match {_format_type(tp)}\"
    )


def _matches(value: Any, tp: Any) -> bool:
    # `Any` / `object` accept anything.
    if tp is Any or tp is object:
        return True
    origin = typing.get_origin(tp)
    if origin is None:
        if tp is None or tp is type(None):
            return value is None
        # Typhon widens int -> float (and bool -> int), so mirror that for a
        # numeric-target cast; otherwise a JSON int cast `as! float` would
        # spuriously fail.
        if tp is float:
            return isinstance(value, (int, float)) and not isinstance(value, complex)
        if tp is complex:
            return isinstance(value, (int, float, complex))
        # `newtype Foo = int` lowers to `typing.NewType`, which is a callable,
        # not a type — `isinstance(tp, type)` is False, so this fell straight
        # through to `return True` and the cast was completely unchecked on
        # the compiled path while the VM rejected it. Unwrap to the base type
        # and check that.
        supertype = getattr(tp, \"__supertype__\", None)
        if supertype is not None:
            return _matches(value, supertype)
        # An `interface` lowers to a `Protocol` subclass, and `isinstance`
        # against a Protocol that is not `@runtime_checkable` *raises*
        # TypeError — so `EXPR as! SomeInterface` could never succeed, it only
        # ever blew up inside the guard. Check structurally instead, which is
        # what an interface means anyway: does the value carry the members?
        protocol_attrs = getattr(tp, \"__protocol_attrs__\", None)
        if protocol_attrs is not None:
            return all(hasattr(value, attr) for attr in protocol_attrs)
        if isinstance(tp, type):
            return isinstance(value, tp)
        # An unrecognised descriptor (e.g. a TypeVar) — be permissive so the
        # cast only ever rejects shapes it can actually prove wrong.
        return True
    args = typing.get_args(tp)
    if origin is typing.Union or origin is types.UnionType:
        return any(_matches(value, arg) for arg in args)
    if origin in (list, set, frozenset):
        if not isinstance(value, origin):
            return False
        return not args or all(_matches(item, args[0]) for item in value)
    if origin is dict:
        if not isinstance(value, dict):
            return False
        if len(args) != 2:
            return True
        key_t, val_t = args
        return all(
            _matches(k, key_t) and _matches(v, val_t) for k, v in value.items()
        )
    if origin is tuple:
        if not isinstance(value, tuple):
            return False
        # `tuple[X, ...]` — homogeneous, any length.
        if len(args) == 2 and args[1] is Ellipsis:
            return all(_matches(item, args[0]) for item in value)
        if len(args) != len(value):
            return False
        return all(_matches(item, arg) for item, arg in zip(value, args))
    # Other parameterised origins (collections.abc.*, etc.) — check the
    # erased origin only; element types are beyond what we model here.
    if isinstance(origin, type):
        return isinstance(value, origin)
    return True


def _format_type(tp: Any) -> str:
    try:
        return str(tp)
    except Exception:  # pragma: no cover - defensive
        return repr(tp)
";

/// Generated `typhon_runtime/traceback.py` — rewrites an uncaught
/// exception's traceback to point at `.ty` source.
///
/// `install()` (called from the entry module's `__main__` block when
/// `[emit] traceback-remap = true`) loads every `.py.map` v2 sidecar from
/// the running script's `.sourcemaps/` directory and installs a
/// `sys.excepthook` that text-rewrites each `File \"…​.py\", line N` frame to
/// the corresponding `.ty` location — the same mapping `tyc trace` applies,
/// but automatically. Frames without a sidecar are left untouched, and any
/// failure falls back to the previous hook, so this can only improve a
/// traceback, never suppress one.
const TYPHON_RUNTIME_TRACEBACK_PY: &str = "\
# generated by tyc — do not edit
\"\"\"Rewrite uncaught-exception tracebacks back to .ty source.\"\"\"
from __future__ import annotations

import json
import os
import re
import sys
import traceback as _tb

_FRAME_RE = re.compile(r'File \"([^\"]+)\", line (\\d+)')


def install() -> None:
    \"\"\"Install a sys.excepthook that maps emitted-.py frames to .ty source.\"\"\"
    state = _load_state()
    if state is None:
        return
    previous = sys.excepthook

    def hook(exc_type, exc, tb):
        try:
            text = ''.join(_tb.format_exception(exc_type, exc, tb))
            sys.stderr.write(_remap(text, state))
        except Exception:  # pragma: no cover - never hide the real error
            previous(exc_type, exc, tb)

    sys.excepthook = hook


def _build_root():
    # The build output root is the directory containing `.sourcemaps/`.
    # Walk up from the entry script so `python -m pkg.main` (entry under a
    # sub-package) still finds it.
    main = sys.modules.get('__main__')
    path = getattr(main, '__file__', None)
    if not path:
        return None
    cur = os.path.dirname(os.path.abspath(path))
    for _ in range(64):
        if os.path.isdir(os.path.join(cur, '.sourcemaps')):
            return cur
        parent = os.path.dirname(cur)
        if parent == cur:
            break
        cur = parent
    return None


def _source_root(build_root):
    # Resolve the real `.ty` source directory by walking up to a typhon.toml
    # and reading [project].src (default 'src') — mirrors `tyc trace`. Returns
    # None for a deployed build with no project file, in which case the bare
    # src-relative source path is shown instead of a fabricated one.
    cur = build_root
    for _ in range(64):
        toml_path = os.path.join(cur, 'typhon.toml')
        if os.path.isfile(toml_path):
            src = 'src'
            try:
                import tomllib

                with open(toml_path, 'rb') as handle:
                    project = tomllib.load(handle).get('project') or {}
                src = project.get('src', 'src')
            except Exception:  # pragma: no cover - defensive
                pass
            return os.path.join(cur, src)
        parent = os.path.dirname(cur)
        if parent == cur:
            break
        cur = parent
    return None


def _load_state():
    build_root = _build_root()
    if build_root is None:
        return None
    sm_dir = os.path.join(build_root, '.sourcemaps')
    # Recurse so package modules (`.sourcemaps/pkg/mod.py.map`) are found, not
    # just top-level ones. Key each map by the emitted `.py` path relative to
    # the build root (e.g. 'pkg/mod.py') so same-named modules don't collide.
    maps = {}
    for dirpath, _dirs, files in os.walk(sm_dir):
        for name in files:
            if not name.endswith('.py.map'):
                continue
            try:
                with open(os.path.join(dirpath, name), encoding='utf-8') as handle:
                    data = json.load(handle)
            except (OSError, ValueError):
                continue
            lines = data.get('lines')
            source = data.get('source')
            if not (isinstance(lines, list) and isinstance(source, str)):
                continue
            rel = os.path.relpath(os.path.join(dirpath, name[:-4]), sm_dir)
            maps[os.path.normpath(rel)] = (source, lines)
    if not maps:
        return None
    return (build_root, maps, _source_root(build_root))


def _remap(text, state):
    build_root, maps, source_root = state

    def repl(match):
        path, lineno = match.group(1), int(match.group(2))
        try:
            rel = os.path.normpath(os.path.relpath(os.path.abspath(path), build_root))
        except ValueError:  # pragma: no cover - different drive on Windows
            return match.group(0)
        entry = maps.get(rel)
        if not entry:
            return match.group(0)
        source, lines = entry
        if not (1 <= lineno <= len(lines)):
            return match.group(0)
        ty_path = os.path.join(source_root, source) if source_root else source
        return 'File \"' + ty_path + '\", line ' + str(lines[lineno - 1])

    return _FRAME_RE.sub(repl, text)
";

/// Render `typhon_runtime/parallel.py`, baking the auto-parallel execution
/// backend (`[strictness] parallel-backend`) in as the module-level
/// `_BACKEND` constant.
///
/// * `"threads"` (the default) runs `map_pure` on a `ThreadPoolExecutor` —
///   order-preserving, and on a free-threaded CPython build (3.13t/3.14t) it
///   escapes the GIL for real parallelism; on the stock GIL build the workers
///   serialise but the results are identical.
/// * `"interpreters"` first tries a PEP 734
///   `concurrent.futures.InterpreterPoolExecutor` (Python 3.14+) and falls
///   back **transparently** to the thread pool on `ImportError` /
///   `AttributeError` (older runtimes) or when the mapped function can't be
///   pickled across the interpreter boundary. Pickling an unshareable
///   callable raises `pickle.PicklingError` / `AttributeError` (a lambda or
///   closure), not the `TypeError` an earlier revision caught — so the
///   generated helper *probes* `pickle.dumps(fn)` up front and falls back on
///   any serialisation failure, without ever wrapping the task execution in
///   a broad `except` (an exception raised *by* `fn` in a worker still
///   propagates exactly as the thread path propagates it). Because the
///   auto-parallel rewrites always pass lambdas, rewritten call sites run on
///   the thread pool even under this backend today; the interpreters pool
///   benefits hand-written `map_pure` calls passing top-level named
///   functions.
///
/// Order is preserved on every path. The `_try_interpreters` helper and the
/// backend switch are emitted for both settings, so the only difference
/// between the two generated files is the `_BACKEND` value — the thread path
/// is behaviourally identical to the historical single-backend runtime.
fn typhon_runtime_parallel_py(backend: &str, min_size: u64) -> String {
    // Config load already validated the value; guard anyway so an unexpected
    // string degrades to the safe thread pool rather than an unknown backend.
    let backend = if backend == "interpreters" {
        "interpreters"
    } else {
        "threads"
    };
    TYPHON_RUNTIME_PARALLEL_PY_TEMPLATE
        .replace("@BACKEND@", backend)
        .replace("@MIN_SIZE@", &min_size.to_string())
}

const TYPHON_RUNTIME_PARALLEL_PY_TEMPLATE: &str = "\
# generated by tyc — do not edit
\"\"\"Parallel-map helpers backing Typhon's auto-parallel rewrite.\"\"\"
from __future__ import annotations

import os
import sys
from concurrent.futures import ThreadPoolExecutor
from typing import Callable, Iterable, TypeVar

_T = TypeVar(\"_T\")
_R = TypeVar(\"_R\")

# Execution backend, baked in at build time from `[strictness] parallel-backend`.
_BACKEND = \"@BACKEND@\"
# Minimum item count worth a thread pool, from `[strictness] parallel-min-size`.
_MIN_SIZE = @MIN_SIZE@


def map_pure(
    fn: Callable[[_T], _R],
    iterable: Iterable[_T],
    *,
    max_workers: int | None = None,
) -> list[_R]:
    \"\"\"Apply ``fn`` to every element of ``iterable``, preserving input order.

    ``fn`` must be pure (the Typhon analyser proves this before emitting calls
    into this helper); calling with a side-effecting function defeats the
    parallelism guarantees.

    With the ``\"threads\"`` backend, or when the ``\"interpreters\"`` backend
    can't be used, work runs on a ``ThreadPoolExecutor``. On a free-threaded
    CPython build (3.13t / 3.14t) the workers run with no GIL contention; on the
    stock CPython build the GIL serialises them — correctness is preserved but
    no speedup is observed. With the ``\"interpreters\"`` backend a PEP 734
    ``InterpreterPoolExecutor`` (Python 3.14+) is tried first, falling back to
    the thread pool transparently. Note: crossing the interpreter boundary
    requires ``fn`` to pickle, and the lambdas Typhon's auto-parallel rewrites
    emit never do — so rewritten call sites always run on the thread pool
    today; the interpreters pool benefits hand-written ``map_pure`` calls that
    pass a top-level named function.
    \"\"\"
    # Materialise once to size the work and to keep ``.map`` from blocking on a
    # slow generator while workers idle.
    items = list(iterable)
    if not items:
        return []
    if max_workers is None:
        max_workers = min(32, (os.cpu_count() or 1) + 4)
    if _BACKEND == \"interpreters\":
        result = _try_interpreters(fn, items, max_workers)
        if result is not None:
            return result
    # Below this point the only backend left is threads, so the two guards the
    # docs have always promised finally apply.
    #
    # They sit *after* `_try_interpreters` deliberately: a sub-interpreter pool
    # gives real parallelism even on a GIL build, so gating it on
    # `_is_gil_enabled()` would disable the one backend that works there.
    #
    # 1. On a GIL build the workers cannot run Python concurrently, so the pool
    #    is pure overhead. The documented `sys._is_gil_enabled()` sequential
    #    fallback did not exist: every rewritten comprehension paid thread
    #    setup for zero parallelism, which measured as a ~60x pessimisation on
    #    stock CPython.
    # 2. Even on a free-threaded build, a handful of items is not worth a pool.
    #    `[strictness] parallel-min-size` gates the *rewrite* at compile time,
    #    but the length is only known here — a comprehension over a runtime
    #    list of 3 took 424x longer through the pool than sequentially.
    gil_enabled = getattr(sys, \"_is_gil_enabled\", None)
    if (gil_enabled is None or gil_enabled()) or len(items) < _MIN_SIZE:
        return [fn(item) for item in items]
    with ThreadPoolExecutor(max_workers=max_workers) as pool:
        return list(pool.map(fn, items))


def _try_interpreters(
    fn: Callable[[_T], _R],
    items: list[_T],
    max_workers: int,
) -> list[_R] | None:
    \"\"\"Attempt a PEP 734 sub-interpreter pool; return ``None`` to fall back.

    Falls back (returns ``None``) when ``InterpreterPoolExecutor`` is
    unavailable (Python < 3.14) or when the mapped function can't cross the
    interpreter boundary. Crossing requires ``fn`` to pickle, and pickling an
    unshareable callable raises ``pickle.PicklingError`` or ``AttributeError``
    (a lambda / closure / local def), sometimes ``TypeError`` — so ``fn`` is
    probed with ``pickle.dumps`` *before* any pool is created, where a failure
    can only mean \"can't serialise fn\". The caller then reruns on the thread
    pool; ``fn`` is pure, so the retry is free of observable effects and order
    is preserved. In particular the lambdas Typhon's auto-parallel rewrites
    emit never pickle, so rewritten call sites always take the thread pool.
    \"\"\"
    try:
        from concurrent.futures import InterpreterPoolExecutor
    except (ImportError, AttributeError):
        return None
    import pickle

    # Pre-flight serialisation probe. Deliberately broad: any failure to
    # pickle ``fn`` means it can't reach a sub-interpreter, whatever the
    # exception type. This must stay a *probe* — never wrap the task
    # execution below in a broad ``except``, or real exceptions raised by
    # ``fn`` inside a worker would be swallowed instead of propagating the
    # way the thread path propagates them.
    try:
        pickle.dumps(fn)
    except Exception:
        return None
    try:
        with InterpreterPoolExecutor(max_workers=max_workers) as pool:
            return list(pool.map(fn, items))
    except (TypeError, pickle.PicklingError):
        # Submission-time serialisation failures only (e.g. an unpicklable
        # *item*); ``fn`` is pure, so retrying on threads is observably
        # identical — and a pure ``fn`` that itself raises one of these
        # re-raises identically on the thread path.
        return None
";

#[cfg(test)]
mod tests {
    use super::*;

    /// Scaffold a minimal project under `dir` and return the src and build paths.
    fn scaffold(
        dir: &std::path::Path,
        src_content: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let src_dir = dir.join("src");
        let out_dir = dir.join("build");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(
            dir.join("typhon.toml"),
            "[project]\nname = \"test\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
             [python]\ntarget = \"3.13\"\n[emit]\nformat = false\n[strictness]\n[env]\n",
        )
        .unwrap();
        std::fs::write(src_dir.join("main.ty"), src_content).unwrap();
        (src_dir, out_dir)
    }

    /// Same as `scaffold`, but with `pgo-memoise = true` and a synthetic
    /// `typhon-profile.json` written at the project root so the Phase-4
    /// PGO path runs end-to-end.
    ///
    /// `profile_entries` is a list of `(qualname, calls)` rows. The function
    /// writes a minimal JSON object matching the schema `tyc profile`
    /// emits.
    fn scaffold_pgo(
        dir: &std::path::Path,
        src_content: &str,
        profile_entries: &[(&str, u64)],
        min_calls: u64,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let src_dir = dir.join("src");
        let out_dir = dir.join("build");
        std::fs::create_dir_all(&src_dir).unwrap();
        let toml = format!(
            "[project]\nname = \"test\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
             [python]\ntarget = \"3.13\"\n[emit]\nformat = false\n\
             [strictness]\npgo-memoise = true\npgo-min-calls = {min_calls}\n[env]\n"
        );
        std::fs::write(dir.join("typhon.toml"), toml).unwrap();
        std::fs::write(src_dir.join("main.ty"), src_content).unwrap();

        // Build the profile JSON by hand to keep this test independent of
        // the runtime helper's serialisation.
        let mut profile = String::from("{");
        for (i, (name, calls)) in profile_entries.iter().enumerate() {
            if i > 0 {
                profile.push(',');
            }
            profile.push_str(&format!(
                "\"{name}\": {{\"calls\": {calls}, \"total_seconds\": 0.001}}"
            ));
        }
        profile.push('}');
        std::fs::write(dir.join("typhon-profile.json"), profile).unwrap();

        (src_dir, out_dir)
    }

    /// Same as `scaffold`, but with `auto-gather = true` set under `[strictness]`
    /// so the Phase-4 auto-gather pass runs end-to-end.
    fn scaffold_auto_gather(
        dir: &std::path::Path,
        src_content: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let src_dir = dir.join("src");
        let out_dir = dir.join("build");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(
            dir.join("typhon.toml"),
            "[project]\nname = \"test\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
             [python]\ntarget = \"3.13\"\n[emit]\nformat = false\n\
             [strictness]\nauto-gather = true\n[env]\n",
        )
        .unwrap();
        std::fs::write(src_dir.join("main.ty"), src_content).unwrap();
        (src_dir, out_dir)
    }

    #[test]
    fn build_emits_stdlib_shadow_warning_for_types_module() {
        // The B3 wiring: `tyc build` reuses the same detection as
        // `tyc check`, so a top-level `types.ty` is flagged while a
        // normally-named `main.ty` is not. We assert against the shared
        // helper directly (the build path prints it to stderr).
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tmp.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("types.ty"), "let kind: str = \"x\"\n").unwrap();
        std::fs::write(src_dir.join("main.ty"), "let x: int = 1\n").unwrap();
        let src_canon = src_dir.canonicalize().unwrap();

        let types_diag = crate::commands::check::check_stdlib_module_shadow(
            &src_dir.join("types.ty"),
            "let kind: str = \"x\"\n",
            &src_canon,
        );
        assert!(
            types_diag.is_some(),
            "a top-level `types.ty` module must produce a stdlib-shadow warning"
        );

        let main_diag = crate::commands::check::check_stdlib_module_shadow(
            &src_dir.join("main.ty"),
            "let x: int = 1\n",
            &src_canon,
        );
        assert!(
            main_diag.is_none(),
            "a normally-named `main.ty` module must NOT warn"
        );
    }

    #[test]
    fn build_with_stdlib_shadow_module_still_succeeds() {
        // The warning is non-fatal: a project containing a top-level
        // `types.ty` must still build and emit `types.py`.
        let tmp = tempfile::tempdir().unwrap();
        let (src_dir, out_dir) = scaffold(tmp.path(), "let x: int = 1\n");
        std::fs::write(src_dir.join("types.ty"), "let kind: str = \"thing\"\n").unwrap();
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .expect("build must succeed despite the stdlib-shadow warning");
        assert!(
            out_dir.join("types.py").exists(),
            "types.py should still be emitted (warning is non-fatal)"
        );
    }

    #[test]
    fn build_produces_py_file_from_simple_source() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, out_dir) = scaffold(tmp.path(), "let greeting: str = \"hello\"\n");
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        assert!(
            out_dir.join("main.py").exists(),
            "main.py should be emitted"
        );
    }

    #[test]
    fn build_out_flag_overrides_config_out_dir() {
        let tmp = tempfile::tempdir().unwrap();
        scaffold(tmp.path(), "let x: int = 42\n");
        let custom_out = tmp.path().join("custom_out");
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: Some(custom_out.clone()),
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        assert!(
            custom_out.join("main.py").exists(),
            "output should go to custom_out/"
        );
    }

    #[test]
    fn build_fails_on_type_error() {
        let tmp = tempfile::tempdir().unwrap();
        scaffold(tmp.path(), "let x: int = \"wrong type\"\n");
        let result = run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        });
        assert!(result.is_err(), "build should fail on type mismatch");
    }

    #[test]
    fn build_pub_star_aggregates_sibling_pub_names_through_init() {
        // R3 follow-up: `pub *` in `__init__.ty` re-exports every
        // sibling `.ty` module's `pub` names through the emitted
        // `__init__.py`. The package's `__all__` is the union of
        // every submodule's `pub` markers, deterministically ordered
        // by submodule basename, and synthesised
        // `from .submodule import …` lines drive runtime resolution.
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tmp.path().join("src");
        let pkg_dir = src_dir.join("mypkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            tmp.path().join("typhon.toml"),
            "[project]\nname = \"pubstar_test\"\nsrc = \"src\"\nout = \"build\"\n\
             [python]\ntarget = \"3.13\"\n[emit]\nformat = false\n\
             [strictness]\nauto-gather = false\n[env]\n",
        )
        .unwrap();
        std::fs::write(src_dir.join("main.ty"), "let x: int = 1\n").unwrap();
        std::fs::write(pkg_dir.join("__init__.ty"), "pub *\n").unwrap();
        std::fs::write(
            pkg_dir.join("widget.ty"),
            "pub class Widget:\n    name: str\n\npub def make_widget(n: str) -> Widget:\n    return Widget(name=n)\n",
        )
        .unwrap();
        std::fs::write(
            pkg_dir.join("util.ty"),
            "pub def shout(s: str) -> str:\n    return s.upper()\n",
        )
        .unwrap();
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        let init_py =
            std::fs::read_to_string(tmp.path().join("build").join("mypkg").join("__init__.py"))
                .unwrap();
        assert!(
            init_py.contains("from .util import shout"),
            "expected sibling re-export `from .util import shout`; got:\n{init_py}"
        );
        assert!(
            init_py.contains("from .widget import Widget, make_widget"),
            "expected sibling re-export `from .widget import Widget, make_widget`; got:\n{init_py}"
        );
        assert!(
            init_py.contains("__all__"),
            "expected __all__ to be synthesised; got:\n{init_py}"
        );
        // Deterministic ordering: util sorts before widget.
        let util_at = init_py
            .find(".util import")
            .expect("util re-export present");
        let widget_at = init_py
            .find(".widget import")
            .expect("widget re-export present");
        assert!(
            util_at < widget_at,
            "siblings must be sorted by basename (util before widget); got:\n{init_py}"
        );
    }

    #[test]
    fn build_pub_star_outside_init_is_a_silent_passthrough() {
        // Outside `__init__.ty`, `pub *` has no effect — the marker
        // is stripped by the preprocessor and the build proceeds as
        // if it weren't there. A warning is emitted via stderr so
        // the user knows the marker is in the wrong place (the
        // warning text isn't asserted here — the integration tests
        // don't capture stderr).
        let tmp = tempfile::tempdir().unwrap();
        let (_, out_dir) = scaffold(tmp.path(), "pub *\n\npub def f() -> int:\n    return 1\n");
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        assert!(
            !py.contains("pub *"),
            "`pub *` line must not appear in the emitted Python: {py}"
        );
        assert!(
            py.contains("def f()"),
            "function definition must still emit cleanly: {py}"
        );
    }

    #[test]
    fn build_emits_typhon_runtime_when_result_used() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, out_dir) = scaffold(tmp.path(), "def f() -> Ok[int]:\n    return Ok(1)\n");
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        // Phase 3 made `typhon_runtime` a package (with submodules `tasks`
        // and `lazy`) rather than a single file. The `__init__.py` is the
        // entry point that still re-exports `Ok` / `Err` / `Result`.
        let pkg = out_dir.join("typhon_runtime");
        assert!(
            pkg.join("__init__.py").exists(),
            "typhon_runtime/__init__.py should be emitted when Ok/Err are used"
        );
        assert!(
            pkg.join("tasks.py").exists(),
            "typhon_runtime/tasks.py should be emitted alongside the package"
        );
        assert!(
            pkg.join("lazy.py").exists(),
            "typhon_runtime/lazy.py should be emitted alongside the package"
        );
    }

    // ── Advanced-feature acceptance fixtures ──────────────────────────────────

    #[test]
    fn build_gather_block_lowers_to_asyncio_task_group() {
        let tmp = tempfile::tempdir().unwrap();
        let src = "\
async def fetch_user(id: int) -> str:
    return \"alice\"

async def fetch_posts(id: int) -> int:
    return 42

async def load(id: int) -> None:
    gather:
        user = fetch_user(id)
        posts = fetch_posts(id)
";
        let (_, out_dir) = scaffold(tmp.path(), src);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        assert!(
            py.contains("TaskGroup"),
            "gather: should lower to asyncio.TaskGroup; got:\n{py}"
        );
        assert!(
            py.contains("create_task"),
            "gather: should emit create_task calls; got:\n{py}"
        );
    }

    #[test]
    fn build_pipe_operator_desugars_to_nested_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let src = "\
def double(x: int) -> int:
    return x * 2

def inc(x: int) -> int:
    return x + 1

let result: int = 3 |> double |> inc
";
        let (_, out_dir) = scaffold(tmp.path(), src);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        // After desugaring `3 |> double |> inc` → `inc(double(3))` the pipe
        // operator itself must not appear in the emitted Python.
        assert!(
            !py.contains("|>"),
            "pipe operator must be desugared away; got:\n{py}"
        );
        // Verify the nested call structure: `inc(double(...))` must appear.
        assert!(
            py.contains("inc(double("),
            "pipe must desugar to inc(double(...)); got:\n{py}"
        );
    }

    #[test]
    fn build_lazy_let_module_level_lowers_to_lazy_let_call() {
        let tmp = tempfile::tempdir().unwrap();
        let src = "lazy let CONFIG: int = 42\nlet first: int = CONFIG\n";
        let (_, out_dir) = scaffold(tmp.path(), src);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        assert!(
            !py.contains("lazy let"),
            "lazy let must be expanded; got:\n{py}"
        );
        // The emitter normalises lambdas to `lambda :`; either spacing is
        // acceptable.
        assert!(
            py.contains("__typhon_lazy_let(lambda: 42)")
                || py.contains("__typhon_lazy_let(lambda : 42)"),
            "module-level lazy let should lower to lazy_let(lambda: …); got:\n{py}"
        );
        assert!(
            py.contains("from typhon_runtime.lazy import lazy_let"),
            "module-level lazy let should inject the runtime import; got:\n{py}"
        );
    }

    #[test]
    fn lazy_value_proxy_forwards_operators() {
        // Regression: a module-level `lazy let` of a primitive emits a
        // `_LazyValue` proxy. The proxy must forward arithmetic, comparison,
        // index and conversion dunders so `VALUE + 1`, `VALUE > 10`,
        // `range(VALUE)` work — not just attribute access. Guards against the
        // forwarding methods being dropped from the runtime template.
        for method in [
            "def __add__",
            "def __radd__",
            "def __sub__",
            "def __mul__",
            "def __mod__",
            "def __lt__",
            "def __gt__",
            "def __ge__",
            "def __index__",
            "def __int__",
            "def __neg__",
            "def __contains__",
        ] {
            assert!(
                TYPHON_RUNTIME_LAZY_PY.contains(method),
                "_LazyValue runtime proxy must forward {method}; lazy let of a \
                 primitive crashes without it"
            );
        }
    }

    #[test]
    fn build_lazy_let_inside_class_lowers_to_cached_property() {
        let tmp = tempfile::tempdir().unwrap();
        let src = "\
class Foo:
    name: str
    lazy let greeting: str = \"hi\"
";
        let (_, out_dir) = scaffold(tmp.path(), src);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        assert!(
            py.contains("cached_property"),
            "class-body lazy let should emit cached_property; got:\n{py}"
        );
        assert!(
            py.contains("def greeting(self) -> str:"),
            "class-body lazy let should produce a method signature; got:\n{py}"
        );
    }

    #[test]
    fn build_lazy_import_expands_to_runtime_helper_call() {
        let tmp = tempfile::tempdir().unwrap();
        // Use `np` after the lazy import so the unused-import check passes.
        let src = "lazy import np = numpy\nlet arr: object = np.array([1, 2, 3])\n";
        let (_, out_dir) = scaffold(tmp.path(), src);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        // The lazy import must be expanded; the raw `lazy import np = …`
        // Typhon syntax must not appear in the emitted Python. (Note the
        // injected runtime header is `from typhon_runtime.lazy import
        // lazy_import as …`, which also contains the substring
        // "lazy import" — so the check is on the full Typhon form.)
        assert!(
            !py.contains("lazy import np ="),
            "lazy import must be expanded; got:\n{py}"
        );
        // Today's emission delegates to the runtime helper — one header
        // import plus one short call per lazy module — instead of a
        // 30-line bespoke proxy class. The runtime wraps the stdlib's
        // `importlib.util.LazyLoader`, so submodule loading and
        // `isinstance(np, ModuleType)` work out of the box.
        assert!(
            py.contains("from typhon_runtime.lazy import lazy_import as __typhon_lazy_import"),
            "lazy import must inject the runtime helper import; got:\n{py}"
        );
        assert!(
            py.contains("np = __typhon_lazy_import(\"numpy\")"),
            "lazy import must lower to a __typhon_lazy_import call; got:\n{py}"
        );
        // The old per-import proxy class is gone — assert that the
        // 30-line bespoke form has not crept back in.
        assert!(
            !py.contains("__TyphonLazy_"),
            "old proxy class form must not be emitted; got:\n{py}"
        );
    }

    #[test]
    fn build_sealed_union_exhaustive_match_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let src = "\
class Circle:
    radius: float

class Square:
    side: float

type Shape = Circle | Square

def area(s: Shape) -> float:
    match s:
        case Circle(radius=r):
            return 3.14 * r * r
        case Square(side=side):
            return side * side
";
        let (_, out_dir) = scaffold(tmp.path(), src);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        assert!(
            py.contains("match"),
            "match statement should appear in emitted Python; got:\n{py}"
        );
    }

    #[test]
    fn build_sealed_union_non_exhaustive_match_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let src = "\
class Circle:
    radius: float

class Square:
    side: float

type Shape = Circle | Square

def area(s: Shape) -> float:
    match s:
        case Circle(radius=r):
            return 3.14 * r * r
";
        scaffold(tmp.path(), src);
        let result = run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        });
        // Verify the failure is specifically a type-checking error, not a
        // configuration or I/O error, by checking the returned error message.
        assert!(
            result.is_err_and(|e| e.to_string().contains("fix type errors")),
            "non-exhaustive match on sealed union should fail with a type error"
        );
    }

    #[test]
    fn build_pure_memo_function_emits_functools_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let src = "\
@memo
def fib(n: int) -> int:
    if n <= 1:
        return n
    return fib(n - 1) + fib(n - 2)
";
        let (_, out_dir) = scaffold(tmp.path(), src);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        assert!(
            py.contains("functools"),
            "@memo should inject functools.cache; got:\n{py}"
        );
        assert!(
            py.contains("cache"),
            "@memo should inject @functools.cache decorator; got:\n{py}"
        );
    }

    #[test]
    fn build_interface_conformance_error_on_missing_member() {
        let tmp = tempfile::tempdir().unwrap();
        // `Dog` does not implement `speak`, so assigning it to `Animal` must
        // fail the structural conformance check.
        let src = "\
interface Animal:
    def speak(self) -> str: ...

class Dog:
    name: str

let pet: Animal = Dog(name=\"Rex\")
";
        scaffold(tmp.path(), src);
        let result = run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        });
        // Verify the failure is specifically a type-checking error (structural
        // conformance failure), not a configuration or I/O error.
        assert!(
            result.is_err_and(|e| e.to_string().contains("fix type errors")),
            "assigning a non-conforming type to an interface variable should fail with a type error"
        );
    }

    // ── Phase 4: profile-guided memoise end-to-end ──────────────────────────

    #[test]
    fn build_pgo_promotes_hot_pure_function_to_cache() {
        // `@pure` function that the profile reports as hot — PGO should add
        // it to the memoise list and the desugarer must emit
        // `@functools.cache`.
        let tmp = tempfile::tempdir().unwrap();
        let src = "\
@pure
def hot(n: int) -> int:
    return n + 1
";
        let (_, out_dir) = scaffold_pgo(tmp.path(), src, &[("main.hot", 5_000)], 100);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        assert!(
            py.contains("functools"),
            "hot @pure function should be cached by PGO; got:\n{py}"
        );
        assert!(
            py.contains("cache"),
            "PGO should inject @functools.cache; got:\n{py}"
        );
    }

    #[test]
    fn build_pgo_leaves_cold_function_uncached() {
        // The profile says `cold` was called once. With min-calls=100 PGO
        // must NOT promote it, so the emitted Python should have no
        // `functools.cache` decorator.
        let tmp = tempfile::tempdir().unwrap();
        let src = "\
@pure
def cold(n: int) -> int:
    return n + 1
";
        let (_, out_dir) = scaffold_pgo(tmp.path(), src, &[("main.cold", 1)], 100);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        assert!(
            !py.contains("@functools.cache"),
            "cold @pure function must NOT be cached by PGO; got:\n{py}"
        );
    }

    #[test]
    fn build_pgo_flag_off_ignores_profile_file() {
        // Same source + profile as the hot-function test, but the project
        // config does not enable PGO. The profile must be ignored.
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tmp.path().join("src");
        let out_dir = tmp.path().join("build");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(
            tmp.path().join("typhon.toml"),
            "[project]\nname = \"t\"\nversion = \"0.1\"\nsrc = \"src\"\nout = \"build\"\n\
             [python]\ntarget = \"3.13\"\n[emit]\nformat = false\n[strictness]\n[env]\n",
        )
        .unwrap();
        std::fs::write(
            src_dir.join("main.ty"),
            "@pure\ndef hot(n: int) -> int:\n    return n + 1\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("typhon-profile.json"),
            "{\"main.hot\": {\"calls\": 5000, \"total_seconds\": 0.5}}",
        )
        .unwrap();
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        assert!(
            !py.contains("@functools.cache"),
            "PGO off => profile must be ignored; got:\n{py}"
        );
    }

    #[test]
    fn build_auto_gather_requires_gatherable_decorator() {
        // With auto-gather enabled but no @gatherable decorators on the
        // callees, the pass must leave the sequential awaits alone.
        // Regression for the Copilot review on auto-gather eligibility.
        let tmp = tempfile::tempdir().unwrap();
        let src = "\
async def fetch_a() -> int:
    return 1
async def fetch_b() -> int:
    return 2

async def load() -> int:
    let a = await fetch_a()
    let b = await fetch_b()
    return a + b
";
        let (_, out_dir) = scaffold_auto_gather(tmp.path(), src);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        assert!(
            !py.contains("TaskGroup"),
            "callees without @gatherable must NOT be gathered; got:\n{py}"
        );
    }

    // ── Phase 4: auto-gather inference end-to-end ───────────────────────────

    #[test]
    fn build_auto_gather_flag_off_keeps_sequential_awaits() {
        let tmp = tempfile::tempdir().unwrap();
        let src = "\
@gatherable
async def fetch_a() -> int:
    return 1
@gatherable
async def fetch_b() -> int:
    return 2

async def load() -> int:
    let a = await fetch_a()
    let b = await fetch_b()
    return a + b
";
        let (_, out_dir) = scaffold(tmp.path(), src);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        assert!(
            !py.contains("TaskGroup"),
            "auto-gather is off — should NOT emit TaskGroup; got:\n{py}"
        );
        assert!(
            py.contains("a = await fetch_a"),
            "sequential await should be preserved; got:\n{py}"
        );
    }

    #[test]
    fn build_auto_gather_flag_on_folds_independent_awaits() {
        let tmp = tempfile::tempdir().unwrap();
        let src = "\
@gatherable
async def fetch_a() -> int:
    return 1
@gatherable
async def fetch_b() -> int:
    return 2

async def load() -> int:
    let a = await fetch_a()
    let b = await fetch_b()
    return a + b
";
        let (_, out_dir) = scaffold_auto_gather(tmp.path(), src);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        assert!(
            py.contains("asyncio.TaskGroup"),
            "auto-gather is on — should emit asyncio.TaskGroup; got:\n{py}"
        );
        assert!(
            py.contains("import asyncio"),
            "should inject `import asyncio` for the rewritten block; got:\n{py}"
        );
        assert!(
            !py.contains("a = await fetch_a"),
            "original sequential await should be folded away; got:\n{py}"
        );
    }

    #[test]
    fn build_auto_gather_respects_data_dependencies() {
        // Second await consumes the first's binding, so the run must NOT
        // be folded even with auto-gather enabled.
        let tmp = tempfile::tempdir().unwrap();
        let src = "\
@gatherable
async def fetch_a() -> int:
    return 1
@gatherable
async def fetch_b(x: int) -> int:
    return x

async def load() -> int:
    let a = await fetch_a()
    let b = await fetch_b(a)
    return a + b
";
        let (_, out_dir) = scaffold_auto_gather(tmp.path(), src);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        assert!(
            !py.contains("TaskGroup"),
            "dependent awaits must NOT be gathered; got:\n{py}"
        );
        assert!(
            py.contains("a = await fetch_a") && py.contains("b = await fetch_b(a)"),
            "sequential awaits must be preserved verbatim; got:\n{py}"
        );
    }

    /// Scaffold a two-file project (`services.ty` + `main.ty`) with
    /// `auto-gather = true`, for the cross-module rewrite tests.
    fn scaffold_auto_gather_xmodule(
        dir: &std::path::Path,
        services_src: &str,
        main_src: &str,
    ) -> std::path::PathBuf {
        let src_dir = dir.join("src");
        let out_dir = dir.join("build");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(
            dir.join("typhon.toml"),
            "[project]\nname = \"test\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
             [python]\ntarget = \"3.13\"\n[emit]\nformat = false\n\
             [strictness]\nauto-gather = true\n[env]\n",
        )
        .unwrap();
        std::fs::write(src_dir.join("services.ty"), services_src).unwrap();
        std::fs::write(src_dir.join("main.ty"), main_src).unwrap();
        out_dir
    }

    #[test]
    fn build_auto_gather_folds_imported_gatherable_run() {
        // Both callees are `@gatherable` in an imported module — the run
        // must fold into a TaskGroup even though neither is declared in
        // `main.ty`. Exercises the cross-module eligibility seed.
        let tmp = tempfile::tempdir().unwrap();
        let services = "\
@gatherable
pub async def fetch_user(uid: int) -> int:
    return uid
@gatherable
pub async def fetch_posts(uid: int) -> int:
    return uid
";
        let main = "\
from services import fetch_user, fetch_posts

async def load(uid: int) -> int:
    let a = await fetch_user(uid)
    let b = await fetch_posts(uid)
    return a + b
";
        let out_dir = scaffold_auto_gather_xmodule(tmp.path(), services, main);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: true,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        assert!(
            py.contains("asyncio.TaskGroup"),
            "imported @gatherable run must fold cross-module; got:\n{py}"
        );
        assert!(
            !py.contains("a = await fetch_user"),
            "original sequential await should be folded away; got:\n{py}"
        );
    }

    #[test]
    fn build_auto_gather_skips_imported_non_gatherable() {
        // `fetch_posts` is NOT `@gatherable` in the imported module — the
        // run must stay sequential, preserving the cross-module safety
        // boundary (the decorator is the author's concurrency attestation).
        let tmp = tempfile::tempdir().unwrap();
        let services = "\
@gatherable
pub async def fetch_user(uid: int) -> int:
    return uid
pub async def fetch_posts(uid: int) -> int:
    return uid
";
        let main = "\
from services import fetch_user, fetch_posts

async def load(uid: int) -> int:
    let a = await fetch_user(uid)
    let b = await fetch_posts(uid)
    return a + b
";
        let out_dir = scaffold_auto_gather_xmodule(tmp.path(), services, main);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: true,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        assert!(
            !py.contains("TaskGroup"),
            "a non-@gatherable imported callee must NOT fold; got:\n{py}"
        );
        assert!(
            py.contains("a = await fetch_user"),
            "sequential awaits must be preserved; got:\n{py}"
        );
    }

    // ── Source map v2 helpers ────────────────────────────────────────────────

    #[test]
    fn build_source_map_v2_produces_correct_json() {
        // Three output lines, all from preprocessed line 2 (offset 6 in "line1\nline2\n")
        let preprocessed = "line1\nline2\n";
        let offsets = vec![0usize, 6, 6];
        let json = build_source_map_v2("main.ty", preprocessed, &offsets, &[]);
        assert!(
            json.contains("\"version\":2"),
            "version must be 2; got: {json}"
        );
        assert!(
            json.contains("\"source\":\"main.ty\""),
            "source field missing; got: {json}"
        );
        assert!(
            json.contains("\"line_strategy\":\"table\""),
            "strategy must be table; got: {json}"
        );
        assert!(
            json.contains("\"lines\":[1,2,2]"),
            "lines array wrong; got: {json}"
        );

        // With a provenance table the same offsets resolve through it: here
        // preprocessed lines 1 and 2 both came from `.ty` line 1.
        let json = build_source_map_v2("main.ty", preprocessed, &offsets, &[0, 0]);
        assert!(
            json.contains("\"lines\":[1,1,1]"),
            "lines array must be remapped through the .ty table; got: {json}"
        );
    }

    #[test]
    fn build_emits_v2_source_map_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, out_dir) = scaffold(tmp.path(), "let x: int = 1\n");
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        let map_path = out_dir.join(".sourcemaps").join("main.py.map");
        assert!(
            map_path.exists(),
            "main.py.map sidecar should be emitted under .sourcemaps/"
        );
        let map = std::fs::read_to_string(&map_path).unwrap();
        assert!(
            map.contains("\"version\":2"),
            "map should be v2; got: {map}"
        );
        assert!(
            map.contains("\"line_strategy\":\"table\""),
            "map strategy should be table; got: {map}"
        );
        assert!(
            map.contains("\"lines\":["),
            "map should contain lines array; got: {map}"
        );
    }

    #[test]
    fn build_interface_conformance_passes_for_conforming_type() {
        let tmp = tempfile::tempdir().unwrap();
        // `Dog` has `speak` in its class body, so it structurally conforms to
        // `Animal`. Note: methods added via a separate `impl Dog:` block are
        // merged only at desugar time, after the type checker runs, so the
        // conformance check requires the method to appear in the class body.
        let src = "\
interface Animal:
    def speak(self) -> str: ...

class Dog:
    name: str
    def speak(self) -> str:
        return \"woof\"

let pet: Animal = Dog(name=\"Rex\")
";
        let (_, out_dir) = scaffold(tmp.path(), src);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        assert!(
            py.contains("speak"),
            "speak method should appear in emitted Python; got:\n{py}"
        );
    }

    // ── Phase 5.4: .py interop in build output ──────────────────────────────

    #[test]
    fn build_copies_stray_py_files_to_output_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let (src_dir, out_dir) =
            scaffold(tmp.path(), "from .helper import foo\nlet x: int = foo()\n");
        std::fs::write(
            src_dir.join("helper.py"),
            "def foo() -> int:\n    return 7\n",
        )
        .unwrap();
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        assert!(
            out_dir.join("main.py").exists(),
            "main.py should be emitted"
        );
        let helper_py = out_dir.join("helper.py");
        assert!(
            helper_py.exists(),
            "helper.py should be copied to the output directory"
        );
        let copied = std::fs::read_to_string(&helper_py).unwrap();
        assert!(
            copied.contains("def foo() -> int:"),
            "copied helper.py should preserve its contents; got:\n{copied}"
        );
    }

    #[test]
    fn build_skips_pycache_directory_when_copying_py_files() {
        let tmp = tempfile::tempdir().unwrap();
        let (src_dir, out_dir) = scaffold(tmp.path(), "let x: int = 1\n");
        let pycache = src_dir.join("__pycache__");
        std::fs::create_dir_all(&pycache).unwrap();
        std::fs::write(pycache.join("stale.cpython-313.pyc"), "binary").unwrap();
        std::fs::write(pycache.join("stale.py"), "x = 1\n").unwrap();
        let tests_dir = src_dir.join("tests");
        std::fs::create_dir_all(&tests_dir).unwrap();
        std::fs::write(tests_dir.join("test_foo.py"), "x = 1\n").unwrap();
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        assert!(
            !out_dir.join("__pycache__").exists(),
            "__pycache__ contents must not be copied"
        );
        assert!(
            !out_dir.join("tests").exists(),
            "tests/ contents must not be copied"
        );
    }

    // ── Phase 5.6: --check dry-run ─────────────────────────────────────────

    #[test]
    fn build_check_does_not_create_files() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, out_dir) = scaffold(tmp.path(), "let x: int = 1\n");
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: true,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        assert!(
            !out_dir.join("main.py").exists(),
            "--check must not write main.py"
        );
        assert!(
            !out_dir.join(".sourcemaps").join("main.py.map").exists(),
            "--check must not write .py.map sidecar"
        );
    }

    #[test]
    fn build_check_reports_intended_outputs() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, out_dir) = scaffold(tmp.path(), "def f() -> Ok[int]:\n    return Ok(1)\n");
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: true,
            no_sync: false,
            with_ty: false,
            optimise: false,
        })
        .unwrap();
        assert!(!out_dir.join("main.py").exists());
        assert!(!out_dir.join("typhon_runtime").exists());
    }

    #[test]
    fn build_check_still_reports_type_errors() {
        let tmp = tempfile::tempdir().unwrap();
        scaffold(tmp.path(), "let x: int = \"wrong type\"\n");
        let result = run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: true,
            no_sync: false,
            with_ty: false,
            optimise: false,
        });
        assert!(
            result.is_err(),
            "type errors must still fail the build under --check"
        );
    }

    // ── Phase 5.6: comptime secret-literal lint ────────────────────────────

    /// Serialises every test that mutates process environment variables.
    /// Rust tests run in parallel by default, and `std::env::set_var` /
    /// `remove_var` mutate global state — concurrent test threads racing
    /// on the same variable produce flaky failures. Holding this mutex
    /// for the duration of an env-mutating test serialises those threads.
    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn build_warns_on_comptime_secret_binding() {
        let _guard = lock_env();
        std::env::set_var("FAKE_API_KEY", "secret");
        let tmp = tempfile::tempdir().unwrap();
        let (_, out_dir) = scaffold(
            tmp.path(),
            "comptime let API_KEY: str = env(\"FAKE_API_KEY\", \"test-value\")\nlet x: str = API_KEY\n",
        );
        let result = run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
            with_ty: false,
            optimise: false,
        });
        std::env::remove_var("FAKE_API_KEY");
        assert!(
            result.is_ok(),
            "secret lint is a warning, not an error: {result:?}"
        );
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        assert!(
            py.contains("secret"),
            "comptime value should be inlined; got:\n{py}"
        );
    }

    // ── Pure helpers ───────────────────────────────────────────────────────

    #[test]
    fn secret_suffix_matches_credential_names() {
        assert_eq!(secret_suffix("API_KEY"), Some("API_KEY"));
        assert_eq!(secret_suffix("MyToken"), Some("TOKEN"));
        assert_eq!(secret_suffix("myTokenValue"), Some("TOKEN"));
        assert_eq!(secret_suffix("DB_PASSWORD"), Some("PASSWORD"));
        assert_eq!(secret_suffix("client_secret"), Some("SECRET"));
        assert_eq!(secret_suffix("PWD"), Some("PWD"));
        assert_eq!(secret_suffix("API_KEY_FOO"), Some("API_KEY"));
        assert_eq!(secret_suffix("FOO_API_KEY_BAR"), Some("API_KEY"));
        assert_eq!(secret_suffix("KEY_APIKEY"), Some("APIKEY"));
        assert_eq!(secret_suffix("APIKEY"), Some("APIKEY"));
        assert_eq!(secret_suffix("APITOKEN"), Some("APITOKEN"));
        assert_eq!(secret_suffix("APISECRET"), Some("APISECRET"));
        assert_eq!(secret_suffix("API_TOKEN"), Some("API_TOKEN"));
        assert_eq!(secret_suffix("TOKEN123"), Some("TOKEN"));
        assert_eq!(secret_suffix("123TOKEN"), Some("TOKEN"));
        assert_eq!(secret_suffix("my123TOKEN"), Some("TOKEN"));
        assert_eq!(secret_suffix("TOKENString"), Some("TOKEN"));
        assert_eq!(secret_suffix("dbPASSWORDString"), Some("PASSWORD"));
        // `PRIVKEY` is reported as itself, not as the bare `KEY` it contains.
        assert_eq!(secret_suffix("PRIVKEY"), Some("PRIVKEY"));
        assert_eq!(secret_suffix("SSH_PRIVKEY"), Some("PRIVKEY"));
        assert_eq!(secret_suffix("PRIVKEY_PEM"), Some("PRIVKEY"));
        assert_eq!(secret_suffix("DATABASE_DSN"), Some("DSN"));
        assert_eq!(secret_suffix("SLACK_WEBHOOK_URL"), Some("WEBHOOK"));
        assert_eq!(secret_suffix("SESSION_COOKIE"), Some("COOKIE"));
        assert_eq!(secret_suffix("CREDENTIALS"), Some("CREDENTIALS"));
        assert_eq!(secret_suffix("AUTHORIZATION"), Some("AUTHORIZATION"));
        assert_eq!(secret_suffix("SIGNING_CERT"), Some("SIGNING"));
    }

    #[test]
    fn secret_suffix_ignores_unrelated_names() {
        assert_eq!(secret_suffix("PORT"), None);
        assert_eq!(secret_suffix("MAX_RETRIES"), None);
        assert_eq!(secret_suffix("USER"), None);
        assert_eq!(secret_suffix("MONKEY"), None);
        assert_eq!(secret_suffix("PASSPORT"), None);
    }

    #[test]
    fn scan_relative_py_imports_picks_up_sibling_imports() {
        let src = "from .helper import foo\nfrom .other import bar, baz\n";
        let imports = scan_relative_py_imports(src);
        let names: Vec<&str> = imports.iter().map(|(n, _, _, _)| n.as_str()).collect();
        assert!(names.contains(&"helper"));
        assert!(names.contains(&"other"));
    }

    #[test]
    fn scan_relative_py_imports_skips_parent_package_form() {
        let src = "from ..pkg import x\nfrom . import y\n";
        let imports = scan_relative_py_imports(src);
        assert!(
            imports.is_empty(),
            "two-dot and bare-dot imports should be ignored: {imports:?}"
        );
    }

    #[test]
    fn overdeep_relative_import_bound_matches_runtime() {
        // A module at package depth D can ascend at most D levels; `dots > D`
        // crashes the emitted Python with ImportError. Regression: the bound
        // was `D + 1`, which let a depth-2 `from ...x` (and a depth-0
        // `from .x`, which has no parent package) slip through to a runtime
        // crash.
        // depth 0: even `from .x` is invalid (no parent package).
        assert_eq!(
            scan_overdeep_relative_imports("from .helper import h\n", 0).len(),
            1,
            "depth-0 `from .` must be flagged"
        );
        // depth 1: `from .x` ok, `from ..x` invalid.
        assert!(scan_overdeep_relative_imports("from .sib import s\n", 1).is_empty());
        assert_eq!(
            scan_overdeep_relative_imports("from ..up import u\n", 1).len(),
            1
        );
        // depth 2: `from ..x` ok, `from ...x` invalid (the reported case).
        assert!(scan_overdeep_relative_imports("from ..pkg import p\n", 2).is_empty());
        assert_eq!(
            scan_overdeep_relative_imports("from ...top import t\n", 2).len(),
            1
        );
    }
}
