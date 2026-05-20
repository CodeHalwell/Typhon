<div align="center">
  <img src="docs-site/src/assets/typhon-logo.png" alt="Typhon — type-safe, optimised, compiles to CPython" width="480" />

  <h1>Typhon</h1>

  <p><strong>A statically-typed, stricter superset of Python that compiles to clean, readable CPython 3.13+ — with no runtime dependency on the toolchain.</strong></p>

  <p>
    <a href="https://github.com/codehalwell/Typhon/actions/workflows/ci.yml"><img src="https://github.com/codehalwell/Typhon/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
    <a href="https://github.com/codehalwell/Typhon/actions/workflows/deploy-docs.yml"><img src="https://github.com/codehalwell/Typhon/actions/workflows/deploy-docs.yml/badge.svg" alt="Docs"></a>
    <a href="https://github.com/codehalwell/Typhon/actions/workflows/vscode-extension.yml"><img src="https://github.com/codehalwell/Typhon/actions/workflows/vscode-extension.yml/badge.svg" alt="VS Code Extension"></a>
    <a href="https://codehalwell.github.io/Typhon/"><img src="https://img.shields.io/badge/docs-codehalwell.github.io%2FTyphon-2563eb" alt="Documentation"></a>
    <img src="https://img.shields.io/badge/license-MIT-yellow.svg" alt="License: MIT">
    <img src="https://img.shields.io/badge/rust-1.94-orange.svg" alt="Rust 1.94">
    <img src="https://img.shields.io/badge/python-3.13%2B-blue.svg" alt="Python 3.13+">
  </p>

  <p>
    <a href="https://codehalwell.github.io/Typhon/"><strong>Documentation</strong></a> ·
    <a href="docs/language.md">Language Reference</a> ·
    <a href="docs/cli.md">CLI Guide</a> ·
    <a href="docs/roadmap.md">Roadmap</a>
  </p>
</div>

---

> Every `.ty` file emits valid, idiomatic `.py`. Not all `.py` is valid Typhon.

The compiler and language server live in a single Rust binary called `tyc`.

## Install on macOS

One-line install (Apple Silicon and Intel both supported):

```bash
curl -sSL https://raw.githubusercontent.com/codehalwell/typhon/main/install.sh | sh
```

This downloads the latest signed release tarball, verifies its SHA-256, and installs `tyc` to `$HOME/.local/bin`. If that directory isn't already on your `PATH`, the installer prints the exact `export PATH=…` line to add to `~/.zshrc` or `~/.bashrc`.

If you download the tarball manually (rather than via the script), clear the macOS Gatekeeper quarantine attribute once before the first run:

```bash
xattr -d com.apple.quarantine ~/.local/bin/tyc
```

`tyc` itself runs anywhere a modern Rust toolchain produces a binary; the emitted Python **requires CPython 3.13 or newer** at runtime. Older targets are rejected at `typhon.toml` load time so projects can't accidentally build code their interpreter won't run.

For a longer-form guide (custom install dir, pinned version, uninstall, troubleshooting) see [docs/install.md](docs/install.md).

## Why

- **Static safety** — non-nullable by default, no implicit `Any`, explicit error handling via `Result[T, E]`.
- **Modern ergonomics** — `let`/`mut`, interfaces, sealed unions with exhaustive matching, guards, pipes, comptime, lazy loading.
- **Clean output** — emits idiomatic Python; deploy like any other Python project, no Typhon runtime to install.
- **First-class tooling** — one binary builds, checks, formats, and runs as an LSP with sub-100 ms incremental feedback.

## Documentation

📖 **The full documentation site lives at [codehalwell.github.io/Typhon](https://codehalwell.github.io/Typhon/)** — browsable guides, references, and tutorials generated from [`docs-site/`](docs-site/).

The canonical design doc is **[the long-term plan](docs/long-term-plan.md)** — goals, architecture, language design, roadmap, and risks in one place.

Focused references:

| | |
|---|---|
| [docs/architecture.md](docs/architecture.md) | Compiler pipeline, crate layout, toolchain choices |
| [docs/language.md](docs/language.md) | Type system, error handling, async, `let`/`mut`, comptime |
| [docs/vm.md](docs/vm.md) | The in-process tree-walking VM (default execution mode for `tyc run`) |
| [docs/cli.md](docs/cli.md) | The `tyc` binary and its subcommands |
| [docs/configuration.md](docs/configuration.md) | `typhon.toml` reference |
| [docs/roadmap.md](docs/roadmap.md) | Phased delivery plan |
| [docs/risks.md](docs/risks.md) | Risks and mitigations |
| [docs/prior-art.md](docs/prior-art.md) | TypeScript, rust-analyzer, ty, Pyrefly, oxc, Ruff |
| [editors/vscode/README.md](editors/vscode/README.md) | VS Code extension — syntax highlighting and LSP client |

## Quick start

```bash
# Build the compiler
cd tyc
cargo build --release

# Scaffold a new project
./target/release/tyc init myapp

# Check and format Typhon source
./target/release/tyc fmt src/
./target/release/tyc check src/
```

## The `tyc` binary

| Command | Purpose |
|---------|---------|
| `tyc build` | Full pipeline: parse, check, analyse, desugar, emit, format |
| `tyc check` | Parse and type-check only — no emission. For CI use |
| `tyc fmt` | Format `.ty` source files in place |
| `tyc lsp` | Run as a Language Server on stdio |
| `tyc init` | Scaffold a new project: `typhon.toml`, `src/`, `tests/` |
| `tyc trace` | Map a Python traceback back to Typhon source via `.py.map` v2 source maps (per-statement `(out_line → ty_line)` table emitted by the printer) |
| `tyc profile` | Build then instrument every top-level function with call-count + wall-clock sampling; writes `typhon-profile.json` on interpreter exit |
| `tyc migrate` | Convert typed Python (`.py`) into Typhon (`.ty`): `Optional[T]`/`T \| None` → `T?`, module-level annotated assigns gain `let`/`mut`, `@dataclass` decorators and `from dataclasses import dataclass` are dropped |
| `tyc ty`      | Build the project and run Astral's `ty` checker against the emitted Python. Requires `ty` to be installed separately (`pip install ty`). |
| `tyc stubtest` | Build the project and run `python -m mypy.stubtest` against every emitted `.pyi`. Complements `tyc check --stubs` (AST diff) by catching dynamically-created members via runtime introspection. Requires `mypy` (`pip install mypy`). |
| `tyc repl`    | Interactive Typhon evaluator. Compiles each block through the full pipeline and executes against a Python interpreter |
| `tyc debug`   | Build the project and launch the emitted Python under a debugger (default `pdb`). Thin v1 — a Typhon-native source-mapping debugger is a Phase-5 item |
| `tyc add` / `tyc remove` / `tyc sync` | Lightweight package-manager surface over `uv`: rewrite `[dependencies]` / `[dev-dependencies]` in `typhon.toml` and run `uv sync` to install |

See [docs/cli.md](docs/cli.md) for the full reference.

## Project status

**Phase 0 — Foundation** substantially complete:

- ✅ Cargo workspace skeleton with `crates/` directories
- ✅ `let`/`mut` keyword tokens (immutable and mutable bindings)
- ✅ `tyc fmt` — parses and validates `.ty` source, normalises whitespace
- ✅ `tyc check` — validates syntax and emits miette diagnostics
- ✅ `tyc init` — scaffolds new projects with `typhon.toml`
- ✅ Ruff parser fork vendored under `tyc/vendor/` with first-class
  `let`/`mut` soft-keyword support and a `Mutability` field on assignment
  AST nodes. All consumer crates now use `ruff_python_ast` via
  `tyc_syntax::parse_module`; the migration off `rustpython-parser` is
  complete — see [`tyc/vendor/README.md`](tyc/vendor/README.md) for details.
- ✅ `tyc ty` — optional integration that builds the project and runs
  Astral's [`ty`](https://github.com/astral-sh/ty) type checker against
  the emitted Python for a second opinion.

**Phase 1 — Core types** complete:

- ✅ Salsa db with `preprocessed_text` and `module_decl_names` queries
- ✅ Name resolution and scope construction; `let`/`mut` enforcement
- ✅ Nominal types: function signatures, assignment compatibility, primitives, classes
- ✅ Non-nullable by default with flow narrowing on `is None` / `is not None` / `isinstance`
- ✅ `T?` syntax sugar for `T | None` in annotations
- ✅ `tyc check` emits "unknown name", "type mismatch", and "nullable use" diagnostics via miette

**Phase 2 — Class and value features** complete:

- ✅ Class emission as `@dataclass(slots=True)`; `model` keyword emits Pydantic with `extra='forbid'` by default
- ✅ Sealed unions and exhaustive `match`
- ✅ `Result[T, E]`, `Ok`/`Err` constructors, generated `typhon_runtime.py` helper
- ✅ `?` operator for `Result` error propagation (context-checked against the enclosing function's return type)
- ✅ `with`-chain Result sequencing with optional `else err:` block
- ✅ `comptime let/mut` with `env()` lookup; required env vars declared in `[env]`
- ✅ `impl` blocks merged into class definitions at desugar
- ✅ `tower-lsp-server` LSP backend: `tyc lsp` publishes diagnostics on `did_open` / `did_change`, serves position-based hover (binding kind + mutability), and answers go-to-definition by jumping to the resolver's recorded declaration site.

**Phase 3 — Structural typing and advanced features** complete:

- ✅ Generics syntax locked to PEP 695 (`def f[T](x: T)`, `type Vec[T] = list[T]`). Type params resolve in scope and are preserved as `Type::TypeVar(name)` through signatures. Call-site bidirectional inference binds typevars from actual arguments (recursively, e.g. `list[T]` against `list[int]` infers `T=int`; conflicting bindings widen to a union) and substitutes them in the return type. Multi-arg constraint solving and bounded type-var checking are wired up; full variance and higher-kinded forms remain partial.
- ✅ `interface Name:` lowers to `class Name(Protocol):` with a structural conformance check on assignment that walks the candidate's MRO and matches field types. `isinstance(x, Interface)` is rejected by default.
- ✅ `unsafe:` lexical region: lowers to `if True:` for scope preservation, and the type checker tracks `unsafe_depth` to suppress diagnostics inside the block so users can interface with untyped Python without fighting the checker. Boundary checks at assignment sites outside the block apply normally.
- ✅ `@pure`/`@memo`/`@pure(memo=True)` decorators trigger the six-condition purity check; memoised functions get `@functools.cache` injected at desugar time. Project-wide opt-in via `[strictness] auto-memoise`.
- ✅ `gather:` lowers to `asyncio.TaskGroup` by default; `gather(strategy="best-effort"):` to `asyncio.gather(..., return_exceptions=True)`.
- ✅ `go f(x)` lowers through `typhon_runtime.tasks.spawn` with a strong-ref task registry.
- ✅ `lazy import np = numpy` lowers to a thread-safe inline proxy class; `lazy from … import …` is rejected. Module-level `lazy let NAME: T = expr` lowers to a sentinel-cached `lazy_let(lambda: expr)`; class-body `lazy let` lowers to `@cached_property`.
- ✅ Pipe operator `a |> f |> g(arg)` desugars to `g(f(a), arg)`.
- ✅ `extend ClassName:` (alias for `impl` on user-defined classes); `extend BUILTIN:` for `str`/`list`/`dict`/… extracts each method to a module-level free function and rewrites call sites whose receiver carries a matching static annotation. No monkey-patching of built-ins.
- ✅ `.dty` stub files compile to PEP 561 `.pyi`. `tyc check --stubs` parses every `.dty` and diffs its surface API (functions, classes, methods, annotated fields, parameter shapes) against the sibling `.ty`/`.py` implementation, emitting `tyc::stub_mismatch` diagnostics for missing-in-impl / missing-in-stub / signature-mismatch findings. A runtime introspection probe (mypy's `stubtest` proper) is still a follow-up.

**Phase 4+ — Beyond v1** in progress:

- ✅ Automatic `asyncio.gather` inference (opt-in via `[strictness] auto-gather`): straight-line runs of two-or-more independent `await` calls inside an `async def` are folded into a `TaskGroup`.
- ✅ Loop parallelisation for pure list comprehensions on free-threaded Python (opt-in via `[strictness] auto-parallel`, threshold `parallel-min-size`).
- ✅ PGO via `tyc profile` (opt-in via `[strictness] pgo-memoise`): `tyc build` reads `typhon-profile.json` and promotes pure functions whose call counts meet `pgo-min-calls` to `@functools.cache`.
- ✅ LSP completions (visible bindings + Typhon keywords + common builtins) and a "Remove unused import" code-action quick-fix.
- ✅ Cross-file go-to-definition across `.ty`/`.py` boundaries via the resolver's Salsa-tracked `resolved_module` query.
- ✅ Attribute resolution against class definitions (`obj.method`, `Class.field`) and re-export-aware import resolution.
- ✅ `tyc migrate` (typed Python → Typhon), `tyc repl` (interactive evaluator), `tyc debug` (pdb launcher), and `tyc add`/`remove`/`sync` (uv-backed package manager).
- ✅ Source-map line accuracy: `.py.map` records a per-statement `(out_line → ty_line)` table consumed by `tyc trace`.

See [docs/roadmap.md](docs/roadmap.md) for the phased plan and [docs/follow-ups-2026-05-17.md](docs/follow-ups-2026-05-17.md) for the remaining tracked follow-ups.

## Configuration

`typhon.toml` at the project root drives every subcommand:

```toml
[project]
name = "myapp"
version = "0.1.0"
src = "src"
out = "build"

[python]
target = "3.13"
free-threaded = false

[emit]
class-default = "dataclass"  # or "pydantic"
format = true

[strictness]
no-implicit-any = true
unused-import = "error"
exhaustive-match = "error"
```

Full reference: [docs/configuration.md](docs/configuration.md).

## Workspace layout

```
tyc/
├── Cargo.toml                  (workspace root)
├── crates/
│   ├── tyc-syntax/             Typhon lexer/parser
│   ├── tyc-db/                 Salsa incremental database
│   ├── tyc-resolve/            Name resolution and scope construction
│   ├── tyc-types/              Nominal type checker with non-null narrowing
│   ├── tyc-analyse/            Purity, async, comptime (Phase 2+)
│   ├── tyc-desugar/            Typhon AST → Python AST (Phase 2+)
│   ├── tyc-emit/               Python codegen
│   ├── tyc-format/             Source formatter
│   ├── tyc-diagnostics/        miette-based diagnostics
│   ├── tyc-lsp/                LSP backend (Phase 2+)
│   └── tyc/                    CLI binary
└── vendor/                     Vendored crates — Typhon's fork of Ruff
    ├── ruff_text_size/         TextSize/TextRange newtypes
    ├── ruff_source_file/       Line-index over a source string
    ├── ruff_python_trivia/     Whitespace + comment helpers
    ├── ruff_python_ast/        Python AST + Typhon's Mutability extension
    └── ruff_python_parser/     Lexer + parser with let/mut soft keywords
```

See [docs/architecture.md](docs/architecture.md) for the pipeline and crate-by-crate breakdown.
