//! `tyc build` — full compilation pipeline.
//!
//! Runs: expand `?` operators → pre-process → parse → type-check →
//!       evaluate comptime → substitute literals → desugar → emit.
//! Writes `.py` files into the output directory, mirroring the source tree.

use std::collections::HashMap;
use std::path::PathBuf;

use clap::Args;
use miette::{miette, Result};
use ruff_python_ast::{
    AtomicNodeIndex, Expr, ExprStringLiteral, ModModule, Stmt, StringLiteral, StringLiteralFlags,
    StringLiteralValue,
};
use ruff_python_parser::parse_expression;
use ruff_text_size::TextRange;

use tyc_analyse::{
    analyse_purity, collect_gatherable_async_fn_names, detect_missed_gathers,
    evaluate_comptime_with_functions, extract_builtin_extensions, load_profile_samples,
    pgo_memoise_targets, purity_diagnostics, rewrite_auto_gather, rewrite_builtin_extension_calls,
    rewrite_parallel_comprehensions, ComptimeValue, ProfileSample,
};
use tyc_db::{check_file_with_imports, extract_shapes_for_path, TycDatabase};
use tyc_desugar::{desugar_module_with, DesugarOptions};
use tyc_diagnostics::{Diagnostics, TycError};
use tyc_emit::{emit_python_with_source_for_target, emit_stub};
use tyc_format::format_source;
use tyc_syntax::preprocess::{
    expand_gather_blocks, expand_go_calls, expand_inline_question_ops, expand_lazy_imports,
    expand_multiline_guards, expand_pipes, expand_question_ops, expand_typed_let_unpack,
    expand_with_chains, line_byte_starts, preprocess,
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
}

pub fn run(args: BuildArgs) -> Result<()> {
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
    if sources_use_model_keyword(&sources) && !config.dependencies.contains_key("pydantic") {
        config
            .dependencies
            .insert("pydantic".to_string(), "*".to_string());
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
                    .or_insert_with(|| extract_shapes_for_path(&file.display().to_string(), &text));
            }
        }
    }
    for (path, source) in &sources {
        let dotted = crate::commands::util::path_to_dotted(path, src_root);
        project_shapes
            .entry(dotted)
            .or_insert_with(|| extract_shapes_for_path(&path.display().to_string(), source));
    }
    // Venv-introspection enrichment: shell to the project's Python
    // and recover real signatures for every third-party class /
    // function the project imports. Without this, calls like
    // `Agent(name="x")` for a `class Agent(*, name, client, …)`
    // would build clean and crash at runtime with `TypeError:
    // missing 1 required positional argument: 'client'`. See
    // `crate::venv_signatures` for the implementation and the
    // allow-list rules. Skipped silently when no Python / venv is
    // reachable — the worst case is the existing behaviour.
    {
        let project_module_set: std::collections::HashSet<String> = sources
            .iter()
            .map(|(path, _)| crate::commands::util::path_to_dotted(path, src_root))
            .collect();
        let allowed_top_level: std::collections::HashSet<String> = config
            .dependencies
            .keys()
            .chain(config.dev_dependencies.keys())
            .cloned()
            .collect();
        crate::venv_signatures::enrich_project_shapes_with_venv(
            std::slice::from_ref(&src_dir),
            &config_dir,
            &project_module_set,
            allowed_top_level,
            &mut project_shapes,
        );
    }
    // Wrap the registry in `Arc` so each per-file `ExternalShapes`
    // snapshot is a cheap refcount bump instead of an O(modules)
    // clone. FINDINGS — copilot review of v0.2.0.
    let project_shapes = std::sync::Arc::new(project_shapes);

    for (path, source) in &sources {
        let file_diags = check_file_with_imports(
            &mut db,
            path.display().to_string(),
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
    let profile_samples: HashMap<String, ProfileSample> = if config.strictness.pgo_memoise {
        load_profile_samples(&config_dir.join("typhon-profile.json"))
    } else {
        HashMap::new()
    };

    // Phase 2: desugar and emit using the already-loaded source text.
    let mut emitted = 0usize;
    let mut needs_runtime = false;

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
        // Note `expand_lazy_imports` runs first so that `lazy import` lines
        // become a full inline proxy class before the other sugar passes see
        // them.
        // The inline `?` pass runs before the end-of-line `?` pass so
        // `Ok(f(x)?)`-shaped sub-expressions get lifted into temps
        // first. O17 / FINDINGS #66 / R3.13 / E9.
        let expanded = expand_question_ops(&expand_inline_question_ops(&expand_pipes(
            &expand_with_chains(&expand_go_calls(&expand_gather_blocks(
                &expand_multiline_guards(&expand_lazy_imports(&expand_typed_let_unpack(source))),
            ))),
        )));
        let prep = preprocess(&expanded);

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
        let purity_findings = analyse_purity(&module, config.strictness.auto_memoise);
        let purity_diags =
            purity_diagnostics(&purity_findings, &path.display().to_string(), source);
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
        if config.strictness.auto_gather {
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
                    path.display().to_string(),
                    &prep.python_source,
                    offset,
                    length,
                );
                eprintln!("{:?}", miette::Report::new_boxed(Box::new(advice)));
            }
            let eligible = collect_gatherable_async_fn_names(&module);
            let _stats = rewrite_auto_gather(&mut module, &eligible);
        }

        // Lower `extend BUILTIN:` blocks (e.g. `extend str:`) into module-
        // level free functions and rewrite call sites whose receiver has a
        // matching type annotation.  Receivers without a static annotation
        // fall through to native attribute access — matching Python's
        // existing semantics for missing methods.
        let (builtin_ext_registry, _ext_stats) = extract_builtin_extensions(&mut module);
        if !builtin_ext_registry.is_empty() {
            let _ = rewrite_builtin_extension_calls(&mut module, &builtin_ext_registry);
        }

        // Phase 4 loop parallelisation: rewrite `[f(x) for x in xs]` runs
        // whose callee is in the pure-function set into thread-pool maps.
        // Combine with `[python] free-threaded = true` for real parallelism;
        // on stock CPython the rewrite still happens but the GIL serialises
        // the workers (correctness preserved, no speedup).
        if config.strictness.auto_parallel {
            let pure_names: std::collections::HashSet<String> = purity_findings
                .iter()
                .filter(|f| f.violation.is_none())
                .map(|f| f.name.clone())
                .collect();
            if !pure_names.is_empty() {
                let stats = rewrite_parallel_comprehensions(
                    &mut module,
                    &pure_names,
                    config.strictness.parallel_min_size,
                );
                if stats.rewrites > 0 {
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
        let (mut python_src, line_offsets) = emit_python_with_source_for_target(
            &desugar_output.module,
            target_minor,
            Some(&prep.python_source),
        );

        // Optionally normalise whitespace in the emitted Python (tabs → spaces,
        // trailing whitespace, final newline).  Full ruff-style reformatting
        // will replace this when the ruff vendor fork lands in Phase 3.
        if do_format {
            let path_str = path.display().to_string();
            if let Ok(result) = format_source(&python_src, &path_str) {
                python_src = result.output;
            }
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

            std::fs::write(&out_file, &python_src)
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
        let map_body = build_source_map_v2(&source_rel, &prep.python_source, &line_offsets);
        if check_mode {
            println!("would write {}", display_relative(&map_path, &project_root));
            would_write_count += 1;
            let _ = map_body;
        } else {
            if let Some(parent) = map_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| miette!("cannot create '{}': {e}", parent.display()))?;
            }
            std::fs::write(&map_path, map_body)
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
                    path.display().to_string(),
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
        let expanded = expand_question_ops(&expand_inline_question_ops(&expand_pipes(
            &expand_with_chains(&expand_go_calls(&expand_gather_blocks(
                &expand_multiline_guards(&expand_lazy_imports(&expand_typed_let_unpack(&source))),
            ))),
        )));
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
            std::fs::write(&out_file, &stub_text)
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
        let files = [
            ("__init__.py", TYPHON_RUNTIME_INIT_PY),
            ("tasks.py", TYPHON_RUNTIME_TASKS_PY),
            ("lazy.py", TYPHON_RUNTIME_LAZY_PY),
            ("stdlib.py", TYPHON_RUNTIME_STDLIB_PY),
            ("result.py", TYPHON_RUNTIME_RESULT_PY),
            ("parallel.py", TYPHON_RUNTIME_PARALLEL_PY),
            ("freeze.py", TYPHON_RUNTIME_FREEZE_PY),
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
                std::fs::write(&path, body)
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
    Ok(())
}

/// Render `path` as a project-root-relative display string when possible,
/// falling back to the absolute path. Used by the `--check` dry-run mode
/// to keep `would write …` lines readable.
fn display_relative(path: &std::path::Path, project_root: &std::path::Path) -> String {
    path.strip_prefix(project_root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

/// Return `Some(suffix)` if `name` looks like a credential identifier.
/// The match is case-insensitive on the trailing token and is suffix-only.
/// Used by the secret-comptime lint.
fn secret_suffix(name: &str) -> Option<&'static str> {
    let upper = name.to_ascii_uppercase();
    // Order matters: check the longest/most specific suffixes first so
    // `MY_PASSWORD` reports `PASSWORD` rather than the shorter `PASS`.
    [
        "PASSWORD", "SECRET", "TOKEN", "API_KEY", "KEY", "PWD", "PASS",
    ]
    .into_iter()
    .find(|candidate| upper.ends_with(candidate))
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

/// Convert a byte `offset` in `source` to a 1-indexed line number.
fn offset_to_line(source: &str, offset: usize) -> u32 {
    let clamped = offset.min(source.len());
    source.as_bytes()[..clamped]
        .iter()
        .filter(|&&b| b == b'\n')
        .count() as u32
        + 1
}

/// Build a v2 `.py.map` JSON body with a full `lines` table.
///
/// `line_offsets[i]` is the byte offset in `preprocessed` that was "active"
/// when output line `i` (0-indexed) was emitted.  Each offset is converted to
/// a 1-indexed line number and the array is serialised inline.  Synthesised
/// lines (offset 0) correctly land on line 1, matching the identity fallback.
fn build_source_map_v2(source_rel: &str, preprocessed: &str, line_offsets: &[usize]) -> String {
    let lines: Vec<u32> = line_offsets
        .iter()
        .map(|&offset| offset_to_line(preprocessed, offset))
        .collect();
    let lines_json = lines
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"version\":2,\"source\":\"{source_rel}\",\"line_strategy\":\"table\",\"lines\":[{lines_json}]}}\n"
    )
}

// ── Comptime literal substitution ─────────────────────────────────────────────

/// Replace the RHS of every top-level annotated assignment whose name appears
/// in `values` with the evaluated compile-time constant.
///
/// This transforms e.g.:
/// ```python
/// PORT: int = int(env("PORT", "8080"))
/// ```
/// into:
/// ```python
/// PORT: int = 8080
/// ```
fn substitute_comptime_literals(
    mut module: ModModule,
    values: &HashMap<String, ComptimeValue>,
    comptime_fn_names: &[String],
) -> ModModule {
    if values.is_empty() && comptime_fn_names.is_empty() {
        return module;
    }
    module.body = module
        .body
        .into_iter()
        // Drop the `def` for any `comptime def` function — those are only
        // callable during build-time evaluation (their bodies typically
        // reference `env(...)` and other comptime-only intrinsics that do
        // not exist at runtime), so leaving them in the emitted Python
        // would surface a `NameError` if anything called them.
        .filter(|stmt| {
            if let Stmt::FunctionDef(f) = stmt {
                !comptime_fn_names.iter().any(|n| n == f.name.as_str())
            } else {
                true
            }
        })
        .map(|stmt| substitute_stmt(stmt, values))
        .collect();
    module
}

fn substitute_stmt(stmt: Stmt, values: &HashMap<String, ComptimeValue>) -> Stmt {
    if let Stmt::AnnAssign(mut ann) = stmt {
        if let Expr::Name(ref n) = *ann.target {
            if let Some(cv) = values.get(n.id.as_str()) {
                ann.value = Some(Box::new(comptime_value_to_expr(cv)));
                return Stmt::AnnAssign(ann);
            }
        }
        Stmt::AnnAssign(ann)
    } else {
        stmt
    }
}

/// Convert a [`ComptimeValue`] to its Python AST expression by
/// round-tripping through the Python expression parser.
fn comptime_value_to_expr(value: &ComptimeValue) -> Expr {
    let literal = value.to_python_literal();
    match parse_expression(&literal) {
        Ok(parsed) => *parsed.into_syntax().body,
        Err(_) => {
            // Fallback: emit as a string-quoted constant.  Should never happen
            // for the value types we produce.
            let lit = StringLiteral {
                range: TextRange::default(),
                node_index: AtomicNodeIndex::NONE,
                value: Box::from(literal.as_str()),
                flags: StringLiteralFlags::empty(),
            };
            Expr::StringLiteral(ExprStringLiteral {
                range: TextRange::default(),
                node_index: AtomicNodeIndex::NONE,
                value: StringLiteralValue::single(lit),
            })
        }
    }
}

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


# Use `typing.Union` rather than PEP 695 `type Result[T, E] = …` so the
# generated runtime loads under Python 3.10 / 3.11 / 3.12 as well as the
# 3.13+ default. The runtime never inspects the alias's generic
# parameters — `isinstance(x, Err)` and the dataclass shape are what the
# rest of the runtime relies on — so dropping the parameters here is
# harmless. Static type checkers still see `Result` as a union of `Ok`
# and `Err`.
from typing import TypeAlias, Union
Result: TypeAlias = Union[Ok, Err]

__all__ = [\"Ok\", \"Err\", \"Result\", \"tasks\", \"lazy\", \"stdlib\", \"result\", \"parallel\"]
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

const TYPHON_RUNTIME_PARALLEL_PY: &str = "\
# generated by tyc — do not edit
\"\"\"Thread-pool helpers backing Typhon's auto-parallel rewrite.\"\"\"
from __future__ import annotations

import os
from concurrent.futures import ThreadPoolExecutor
from typing import Callable, Iterable, TypeVar

_T = TypeVar(\"_T\")
_R = TypeVar(\"_R\")


def map_pure(
    fn: Callable[[_T], _R],
    iterable: Iterable[_T],
    *,
    max_workers: int | None = None,
) -> list[_R]:
    \"\"\"Apply ``fn`` to every element of ``iterable`` across a thread pool.

    The result list preserves input order. ``fn`` must be pure (the
    Typhon analyser proves this before emitting calls into this
    helper); calling with a side-effecting function defeats the
    parallelism guarantees.

    On a free-threaded CPython build (3.13t / 3.14t) workers run with
    no GIL contention. On the stock CPython build the GIL serialises
    the workers — correctness is preserved but no speedup is observed.
    \"\"\"
    # Materialise once to size the work and to keep ``ThreadPoolExecutor.map``
    # from blocking on a slow generator while workers idle.
    items = list(iterable)
    if not items:
        return []
    if max_workers is None:
        max_workers = min(32, (os.cpu_count() or 1) + 4)
    with ThreadPoolExecutor(max_workers=max_workers) as pool:
        return list(pool.map(fn, items))
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
    fn build_produces_py_file_from_simple_source() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, out_dir) = scaffold(tmp.path(), "let greeting: str = \"hello\"\n");
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
            check: false,
            no_sync: false,
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
        });
        assert!(result.is_err(), "build should fail on type mismatch");
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

    // ── Source map v2 helpers ────────────────────────────────────────────────

    #[test]
    fn offset_to_line_empty_offset_is_line_one() {
        assert_eq!(offset_to_line("hello\nworld\n", 0), 1);
    }

    #[test]
    fn offset_to_line_after_first_newline() {
        // "hello\n" is 6 bytes; byte 6 is the start of "world"
        assert_eq!(offset_to_line("hello\nworld\n", 6), 2);
    }

    #[test]
    fn offset_to_line_clamps_past_end() {
        let src = "a\nb\n";
        assert_eq!(offset_to_line(src, 999), 3);
    }

    #[test]
    fn build_source_map_v2_produces_correct_json() {
        // Three output lines, all from preprocessed line 2 (offset 6 in "line1\nline2\n")
        let preprocessed = "line1\nline2\n";
        let offsets = vec![0usize, 6, 6];
        let json = build_source_map_v2("main.ty", preprocessed, &offsets);
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
        assert_eq!(secret_suffix("DB_PASSWORD"), Some("PASSWORD"));
        assert_eq!(secret_suffix("client_secret"), Some("SECRET"));
        assert_eq!(secret_suffix("PWD"), Some("PWD"));
    }

    #[test]
    fn secret_suffix_ignores_unrelated_names() {
        assert_eq!(secret_suffix("PORT"), None);
        assert_eq!(secret_suffix("MAX_RETRIES"), None);
        assert_eq!(secret_suffix("USER"), None);
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
}
