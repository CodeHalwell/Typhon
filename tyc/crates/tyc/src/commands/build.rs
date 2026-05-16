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

use tyc_analyse::{analyse_purity, evaluate_comptime, purity_diagnostics, ComptimeValue};
use tyc_db::{check_file, TycDatabase};
use tyc_desugar::{desugar_module_with, DesugarOptions};
use tyc_diagnostics::Diagnostics;
use tyc_emit::{emit, emit_stub};
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
        let memoise_targets: Vec<String> = purity_findings
            .iter()
            .filter(|f| f.violation.is_none() && f.memoise)
            .map(|f| f.name.clone())
            .collect();

        let desugar_output = desugar_module_with(
            &module,
            DesugarOptions {
                memoise_functions: memoise_targets,
            },
        );
        if desugar_output.needs_typhon_runtime {
            needs_runtime = true;
        }
        let mut python_src = emit(&desugar_output.module);

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
            &expand_gather_blocks(&source),
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
}
