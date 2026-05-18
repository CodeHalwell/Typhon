# Typhon

A statically-typed, stricter superset of Python that compiles to clean, readable CPython 3.13+ code with no runtime dependency on the toolchain.

> Every `.ty` file emits valid, idiomatic `.py`. Not all `.py` is valid Typhon.

The compiler and language server live in a single Rust binary called `tyc`.

## Why

- **Static safety** — non-nullable by default, no implicit `Any`, explicit error handling via `Result[T, E]`.
- **Modern ergonomics** — `let`/`mut`, interfaces, sealed unions with exhaustive matching, guards, pipes, comptime, lazy loading.
- **Clean output** — emits idiomatic Python; deploy like any other Python project, no Typhon runtime to install.
- **First-class tooling** — one binary builds, checks, formats, and runs as an LSP with sub-100 ms incremental feedback.

## Documentation

The canonical design doc is **[the long-term plan](docs/long-term-plan.md)** — goals, architecture, language design, roadmap, and risks in one place.

Focused references:

| | |
|---|---|
| [docs/architecture.md](docs/architecture.md) | Compiler pipeline, crate layout, toolchain choices |
| [docs/language.md](docs/language.md) | Type system, error handling, async, `let`/`mut`, comptime |
| [docs/cli.md](docs/cli.md) | The `tyc` binary and its subcommands |
| [docs/configuration.md](docs/configuration.md) | `typhon.toml` reference |
| [docs/roadmap.md](docs/roadmap.md) | Phased delivery plan |
| [docs/risks.md](docs/risks.md) | Risks and mitigations |
| [docs/prior-art.md](docs/prior-art.md) | TypeScript, rust-analyzer, ty, Pyrefly, oxc, Ruff |

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
| `tyc trace` | Map a Python traceback back to Typhon source via `.py.map` files (v1: filename rewrite, line offsets are 1:1) |
| `tyc profile` | Build then instrument every top-level function with call-count + wall-clock sampling; writes `typhon-profile.json` on interpreter exit |
| `tyc migrate` | Convert typed Python (`.py`) into Typhon (`.ty`): `Optional[T]`/`T \| None` → `T?`, module-level annotated assigns gain `val`/`var`, `@dataclass` decorators and `from dataclasses import dataclass` are dropped |
| `tyc ty`      | Build the project and run Astral's `ty` checker against the emitted Python. Requires `ty` to be installed separately (`pip install ty`). |

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

**Phase 3 — Structural typing and advanced features** substantially complete:

- ✅ Generics syntax locked to PEP 695 (`def f[T](x: T)`, `type Vec[T] = list[T]`). Type params resolve in scope and are preserved as `Type::TypeVar(name)` through signatures. Call-site bidirectional inference binds typevars from actual arguments (recursively, e.g. `list[T]` against `list[int]` infers `T=int`; conflicting bindings widen to a union) and substitutes them in the return type. Multi-arg constraint solving and bounded type vars are still partial.
- ✅ `interface Name:` lowers to `class Name(Protocol):` with a structural conformance check on assignment. `isinstance(x, Interface)` is rejected by default.
- ✅ `unsafe:` lexical region: lowers to `if True:` for scope preservation, and the type checker tracks `unsafe_depth` to suppress diagnostics inside the block so users can interface with untyped Python without fighting the checker. Boundary checks at assignment sites outside the block apply normally.
- ✅ `@pure`/`@memo`/`@pure(memo=True)` decorators trigger the six-condition purity check; memoised functions get `@functools.cache` injected at desugar time. Project-wide opt-in via `[strictness] auto-memoise`.
- ✅ `gather:` lowers to `asyncio.TaskGroup` by default; `gather(strategy="best-effort"):` to `asyncio.gather(..., return_exceptions=True)`.
- ✅ `go f(x)` lowers through `typhon_runtime.tasks.spawn` with a strong-ref task registry.
- ✅ `lazy import np = numpy` lowers to a thread-safe inline proxy class; `lazy from … import …` is rejected. Module-level `lazy let NAME: T = expr` lowers to `lazy_val(lambda: expr)`; class-body `lazy let` lowers to `@cached_property`.
- ✅ Pipe operator `a |> f |> g(arg)` desugars to `g(f(a), arg)`.
- ✅ `extend ClassName:` (alias for `impl` on user-defined classes; built-in extensions deferred).
- ✅ `.dty` stub files compile to PEP 561 `.pyi`. `tyc check --stubs` parses every `.dty` and diffs its surface API (functions, classes, methods, annotated fields) against the sibling `.ty`/`.py` implementation, emitting `tyc::stub_mismatch` diagnostics for missing-in-impl / missing-in-stub / signature-mismatch findings. A runtime introspection probe (mypy's `stubtest` proper) is still a follow-up.

See [docs/roadmap.md](docs/roadmap.md) for the phased plan through Phase 3 (month twelve) and beyond.

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
    └── ruff_python_parser/     Lexer + parser with val/var soft keywords
```

See [docs/architecture.md](docs/architecture.md) for the pipeline and crate-by-crate breakdown.
