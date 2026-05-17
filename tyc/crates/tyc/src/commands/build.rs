//! `tyc build` — full compilation pipeline.
//!
//! Runs: expand `?` operators → pre-process → parse → type-check →
//!       evaluate comptime → substitute literals → desugar → emit.
//! Writes `.py` files into the output directory, mirroring the source tree.

use std::collections::HashMap;
use std::path::PathBuf;

use clap::Args;
use miette::{miette, Result};
use rustpython_ast::{text_size::TextRange, Constant, Expr, ExprConstant, Mod, Stmt};
use rustpython_parser::{parse, Mode};

use tyc_analyse::{
    analyse_purity, collect_gatherable_async_fn_names, evaluate_comptime, load_profile_samples,
    pgo_memoise_targets, purity_diagnostics, rewrite_auto_gather, ComptimeValue, ProfileSample,
};
use tyc_db::{check_file, TycDatabase};
use tyc_desugar::{desugar_module_with, DesugarOptions};
use tyc_diagnostics::Diagnostics;
use tyc_emit::{emit_stub, emit_with_line_offsets};
use tyc_format::format_source;
use tyc_syntax::preprocess::{
    expand_gather_blocks, expand_go_calls, expand_lazy_imports, expand_pipes, expand_question_ops,
    expand_with_chains, preprocess,
};

use crate::commands::util::{apply_strictness, collect_dty_files, collect_ty_files};
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

    // Phase 1: type-check all files first and fail fast on errors.
    let mut db = TycDatabase::new();
    let mut all_phase1_diags = Diagnostics::new();

    for (path, source) in &sources {
        let file_diags = check_file(&mut db, path.display().to_string(), source.clone());
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
        let expanded = expand_question_ops(&expand_pipes(&expand_with_chains(&expand_go_calls(
            &expand_gather_blocks(&expand_lazy_imports(source)),
        ))));
        let prep = preprocess(&expanded);

        let module = parse(
            &prep.python_source,
            Mode::Module,
            &path.display().to_string(),
        )
        .map_err(|e| miette!("parse error in '{}': {e}", path.display()))?;

        // Evaluate all `comptime` bindings and substitute their literals into
        // the AST before desugaring.
        let (comptime_values, comptime_diags) = evaluate_comptime(&module, &prep.comptime_bindings);
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

        let module = substitute_comptime_literals(module, &comptime_values);

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
        let module = if config.strictness.auto_gather {
            let eligible = collect_gatherable_async_fn_names(&module);
            let (rewritten, _stats) = rewrite_auto_gather(module, &eligible);
            rewritten
        } else {
            module
        };

        let desugar_output = desugar_module_with(
            &module,
            DesugarOptions {
                memoise_functions: memoise_targets,
            },
        );
        if desugar_output.needs_typhon_runtime {
            needs_runtime = true;
        }
        let (mut python_src, line_offsets) = emit_with_line_offsets(&desugar_output.module);

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

        if let Some(parent) = out_file.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| miette!("cannot create '{}': {e}", parent.display()))?;
        }

        std::fs::write(&out_file, &python_src)
            .map_err(|e| miette!("cannot write '{}': {e}", out_file.display()))?;

        // Emit a v2 `.py.map` sidecar alongside the emitted `.py`.
        //
        // The `lines` array maps each Python output line (0-indexed) to a
        // 1-indexed line number in the preprocessed Typhon source.  For most
        // constructs the mapping is identity; sugar that emits multiple Python
        // lines from one Typhon line (e.g. `?`, `gather:`, `with`-chains)
        // correctly maps those lines back to the single originating line.
        let map_path = out_file.with_extension("py.map");
        let source_rel = escape_json_path(
            &path
                .strip_prefix(&src_dir)
                .unwrap_or(path)
                .display()
                .to_string(),
        );
        let map_body = build_source_map_v2(&source_rel, &prep.python_source, &line_offsets);
        std::fs::write(&map_path, map_body)
            .map_err(|e| miette!("cannot write '{}': {e}", map_path.display()))?;

        emitted += 1;
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
        let expanded = expand_question_ops(&expand_pipes(&expand_with_chains(&expand_go_calls(
            &expand_gather_blocks(&expand_lazy_imports(&source)),
        ))));
        let prep = preprocess(&expanded);
        let module = parse(
            &prep.python_source,
            Mode::Module,
            &path.display().to_string(),
        )
        .map_err(|e| miette!("parse error in '{}': {e}", path.display()))?;
        let desugar = desugar_module_with(&module, DesugarOptions::default());
        let stub_text = emit_stub(&desugar.module);

        let rel = path
            .strip_prefix(&src_dir)
            .map_err(|_| miette!("'{}' is outside the source directory", path.display()))?;
        let out_file = out_dir.join(rel).with_extension("pyi");
        if let Some(parent) = out_file.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| miette!("cannot create '{}': {e}", parent.display()))?;
        }
        std::fs::write(&out_file, &stub_text)
            .map_err(|e| miette!("cannot write '{}': {e}", out_file.display()))?;
        stubs_emitted += 1;
    }
    if stubs_emitted > 0 {
        println!("emitted {} stub(s) (.pyi)", stubs_emitted);
    }

    // Emit the typhon_runtime helper alongside the Python output when any
    // source file uses Ok, Err, Result, `go`, `lazy`, etc.  The helper is a
    // generated package the build owns; users do not need to install a
    // separate PyPI package.
    if needs_runtime {
        std::fs::create_dir_all(&out_dir)
            .map_err(|e| miette!("cannot create output dir '{}': {e}", out_dir.display()))?;
        let runtime_dir = out_dir.join("typhon_runtime");
        std::fs::create_dir_all(&runtime_dir)
            .map_err(|e| miette!("cannot create '{}': {e}", runtime_dir.display()))?;
        let files = [
            ("__init__.py", TYPHON_RUNTIME_INIT_PY),
            ("tasks.py", TYPHON_RUNTIME_TASKS_PY),
            ("lazy.py", TYPHON_RUNTIME_LAZY_PY),
        ];
        for (name, body) in files {
            let path = runtime_dir.join(name);
            std::fs::write(&path, body)
                .map_err(|e| miette!("cannot write '{}': {e}", path.display()))?;
        }
        println!("wrote typhon_runtime/ → '{}'", runtime_dir.display());
    }

    println!("built {} file(s) → '{}'", emitted, out_dir.display());
    Ok(())
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
    module: Mod<TextRange>,
    values: &HashMap<String, ComptimeValue>,
) -> Mod<TextRange> {
    if values.is_empty() {
        return module;
    }
    let Mod::Module(mut m) = module else {
        return module;
    };
    m.body = m
        .body
        .into_iter()
        .map(|stmt| substitute_stmt(stmt, values))
        .collect();
    Mod::Module(m)
}

fn substitute_stmt(
    stmt: Stmt<TextRange>,
    values: &HashMap<String, ComptimeValue>,
) -> Stmt<TextRange> {
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

/// Convert a [`ComptimeValue`] to its Python AST constant expression
/// by round-tripping through the Python parser.
fn comptime_value_to_expr(value: &ComptimeValue) -> Expr<TextRange> {
    let literal = value.to_python_literal();
    match parse(&literal, Mode::Expression, "<comptime>") {
        Ok(Mod::Expression(e)) => *e.body,
        _ => {
            // Fallback: emit as a string-quoted constant.  Should never happen
            // for the value types we produce.
            Expr::Constant(ExprConstant {
                range: TextRange::default(),
                value: Constant::Str(literal),
                kind: None,
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

from . import lazy, tasks  # re-exported for `typhon_runtime.lazy.…` / `.tasks.…`

_T = TypeVar(\"_T\")
_E = TypeVar(\"_E\")


@dataclass(slots=True)
class Ok(Generic[_T]):
    value: _T


@dataclass(slots=True)
class Err(Generic[_E]):
    error: _E


type Result[T, E] = Ok[T] | Err[E]

__all__ = [\"Ok\", \"Err\", \"Result\", \"tasks\", \"lazy\"]
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
/// attribute access. `lazy_val(factory)` returns a proxy that materialises the
/// underlying value on first attribute access (and forwards attribute lookups,
/// item subscripts, and calls transparently afterwards).
const TYPHON_RUNTIME_LAZY_PY: &str = "\
# generated by tyc — do not edit
\"\"\"Helpers backing the `lazy import` and `lazy val` Typhon keywords.\"\"\"
from __future__ import annotations

import importlib
import importlib.util
import threading
from types import ModuleType
from typing import Callable, TypeVar

_T = TypeVar(\"_T\")


def lazy_import(name: str) -> ModuleType:
    \"\"\"Return a module proxy that defers loading until first attribute access.

    Built on `importlib.util.LazyLoader`, which installs a deferred-loader
    spec so the first attribute lookup triggers the real import.
    \"\"\"
    spec = importlib.util.find_spec(name)
    if spec is None or spec.loader is None:
        raise ImportError(f\"cannot resolve lazy import {name!r}\")
    loader = importlib.util.LazyLoader(spec.loader)
    spec.loader = loader
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


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


_UNSET = object()


def lazy_val(factory: Callable[[], _T]) -> _T:
    \"\"\"Return a proxy that calls *factory* on first attribute access.\"\"\"
    # Cast for the type checker; behaviour-wise the proxy forwards everything.
    return _LazyValue(factory)  # type: ignore[return-value]
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
        let (_, out_dir) = scaffold(tmp.path(), "val greeting: str = \"hello\"\n");
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
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
        scaffold(tmp.path(), "val x: int = 42\n");
        let custom_out = tmp.path().join("custom_out");
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: Some(custom_out.clone()),
            no_format: true,
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
        scaffold(tmp.path(), "val x: int = \"wrong type\"\n");
        let result = run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
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

val result: int = 3 |> double |> inc
";
        let (_, out_dir) = scaffold(tmp.path(), src);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
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
    fn build_lazy_val_module_level_lowers_to_lazy_val_call() {
        let tmp = tempfile::tempdir().unwrap();
        let src = "lazy val CONFIG: int = 42\nval first: int = CONFIG\n";
        let (_, out_dir) = scaffold(tmp.path(), src);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        assert!(
            !py.contains("lazy val"),
            "lazy val must be expanded; got:\n{py}"
        );
        // The emitter normalises lambdas to `lambda :`; either spacing is
        // acceptable.
        assert!(
            py.contains("__typhon_lazy_val(lambda: 42)")
                || py.contains("__typhon_lazy_val(lambda : 42)"),
            "module-level lazy val should lower to lazy_val(lambda: …); got:\n{py}"
        );
        assert!(
            py.contains("from typhon_runtime.lazy import lazy_val"),
            "module-level lazy val should inject the runtime import; got:\n{py}"
        );
    }

    #[test]
    fn build_lazy_val_inside_class_lowers_to_cached_property() {
        let tmp = tempfile::tempdir().unwrap();
        let src = "\
class Foo:
    name: str
    lazy val greeting: str = \"hi\"
";
        let (_, out_dir) = scaffold(tmp.path(), src);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        assert!(
            py.contains("cached_property"),
            "class-body lazy val should emit cached_property; got:\n{py}"
        );
        assert!(
            py.contains("def greeting(self) -> str:"),
            "class-body lazy val should produce a method signature; got:\n{py}"
        );
    }

    #[test]
    fn build_lazy_import_expands_to_proxy() {
        let tmp = tempfile::tempdir().unwrap();
        // Use `np` after the lazy import so the unused-import check passes.
        let src = "lazy import np = numpy\nval arr: object = np.array([1, 2, 3])\n";
        let (_, out_dir) = scaffold(tmp.path(), src);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        // The lazy import must be expanded to a proxy class; the raw `lazy import`
        // syntax must not appear in the emitted Python.
        assert!(
            !py.contains("lazy import"),
            "lazy import must be expanded; got:\n{py}"
        );
        // `expand_lazy_imports` generates `class __TyphonLazy_{alias}_` as the
        // proxy class name — assert on this specific marker so the test would
        // catch a regression to a plain `import numpy as np` instead.
        assert!(
            py.contains("__TyphonLazy_np_"),
            "lazy import must emit the __TyphonLazy_np_ proxy class; got:\n{py}"
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

val pet: Animal = Dog(name=\"Rex\")
";
        scaffold(tmp.path(), src);
        let result = run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
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
    a = await fetch_a()
    b = await fetch_b()
    return a + b
";
        let (_, out_dir) = scaffold_auto_gather(tmp.path(), src);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
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
    a = await fetch_a()
    b = await fetch_b()
    return a + b
";
        let (_, out_dir) = scaffold(tmp.path(), src);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
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
    a = await fetch_a()
    b = await fetch_b()
    return a + b
";
        let (_, out_dir) = scaffold_auto_gather(tmp.path(), src);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
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
    a = await fetch_a()
    b = await fetch_b(a)
    return a + b
";
        let (_, out_dir) = scaffold_auto_gather(tmp.path(), src);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
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
        let (_, out_dir) = scaffold(tmp.path(), "val x: int = 1\n");
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
        })
        .unwrap();
        let map_path = out_dir.join("main.py.map");
        assert!(map_path.exists(), "main.py.map sidecar should be emitted");
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

val pet: Animal = Dog(name=\"Rex\")
";
        let (_, out_dir) = scaffold(tmp.path(), src);
        run(BuildArgs {
            path: tmp.path().to_path_buf(),
            out: None,
            no_format: true,
        })
        .unwrap();
        let py = std::fs::read_to_string(out_dir.join("main.py")).unwrap();
        assert!(
            py.contains("speak"),
            "speak method should appear in emitted Python; got:\n{py}"
        );
    }
}
