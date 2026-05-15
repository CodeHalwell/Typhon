# Typhon

A statically-typed, stricter superset of Python that compiles to clean, readable CPython 3.13+ code with no runtime dependency on the toolchain.

> Every `.tt` file emits valid, idiomatic `.py`. Not all `.py` is valid Typhon.

The compiler and language server live in a single Rust binary called `ttc`.

## Why

- **Static safety** — non-nullable by default, no implicit `Any`, explicit error handling via `Result[T, E]`.
- **Modern ergonomics** — `val`/`var`, interfaces, sealed unions with exhaustive matching, guards, pipes, comptime, lazy loading.
- **Clean output** — emits idiomatic Python; deploy like any other Python project, no Typhon runtime to install.
- **First-class tooling** — one binary builds, checks, formats, and runs as an LSP with sub-100 ms incremental feedback.

## Documentation

The canonical design doc is **[the long-term plan](docs/long-term-plan.md)** — goals, architecture, language design, roadmap, and risks in one place.

Focused references:

| | |
|---|---|
| [docs/architecture.md](docs/architecture.md) | Compiler pipeline, crate layout, toolchain choices |
| [docs/language.md](docs/language.md) | Type system, error handling, async, `val`/`var`, comptime |
| [docs/cli.md](docs/cli.md) | The `ttc` binary and its subcommands |
| [docs/configuration.md](docs/configuration.md) | `typhon.toml` reference |
| [docs/roadmap.md](docs/roadmap.md) | Phased delivery plan |
| [docs/risks.md](docs/risks.md) | Risks and mitigations |
| [docs/prior-art.md](docs/prior-art.md) | TypeScript, rust-analyzer, ty, Pyrefly, oxc, Ruff |

## Quick start

```bash
# Build the compiler
cd ttc
cargo build --release

# Scaffold a new project
./target/release/ttc init myapp

# Check and format Typhon source
./target/release/ttc fmt src/
./target/release/ttc check src/
```

## The `ttc` binary

| Command | Purpose |
|---------|---------|
| `ttc build` | Full pipeline: parse, check, analyse, desugar, emit, format |
| `ttc check` | Parse and type-check only — no emission. For CI use |
| `ttc fmt` | Format `.tt` source files in place |
| `ttc lsp` | Run as a Language Server on stdio |
| `ttc init` | Scaffold a new project: `typhon.toml`, `src/`, `tests/` |
| `ttc trace` | Map a Python traceback back to Typhon source via `.py.map` files |
| `ttc profile` | Instrument emitted code for hot-function detection (opt-in) |

See [docs/cli.md](docs/cli.md) for the full reference.

## Project status

**Phase 0 — Foundation** is in progress:

- ✅ Cargo workspace skeleton with `crates/` and `vendor/` directories
- ✅ `val`/`var` keyword tokens (immutable and mutable bindings)
- ✅ `ttc fmt` — parses and validates `.tt` source, normalises whitespace
- ✅ `ttc check` — validates syntax and emits miette diagnostics
- ✅ `ttc init` — scaffolds new projects with `typhon.toml`

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
ttc/
├── Cargo.toml                  (workspace root)
├── crates/
│   ├── ttc-syntax/             Typhon lexer/parser
│   ├── ttc-db/                 Salsa database (Phase 1+)
│   ├── ttc-resolve/            Name resolution (Phase 1+)
│   ├── ttc-types/              Type checker (Phase 1+)
│   ├── ttc-analyse/            Purity, async, comptime (Phase 2+)
│   ├── ttc-desugar/            Typhon AST → Python AST (Phase 2+)
│   ├── ttc-emit/               Python codegen
│   ├── ttc-format/             Source formatter
│   ├── ttc-diagnostics/        miette-based diagnostics
│   ├── ttc-lsp/                LSP backend (Phase 2+)
│   └── ttc/                    CLI binary
└── vendor/                     Vendored crates (ruff fork, Phase 1)
```

See [docs/architecture.md](docs/architecture.md) for the pipeline and crate-by-crate breakdown.
