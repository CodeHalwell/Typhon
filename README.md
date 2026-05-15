# Typhon

A statically-typed superset of Python that compiles to clean, readable CPython 3.13+ code with no runtime dependency on the toolchain.

## Overview

Every `.tt` file emits valid, idiomatic `.py`. Not all `.py` is valid Typhon. The compiler and language server live in a single Rust binary called `ttc`.

## Quick Start

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

## The `ttc` Binary

| Command | Purpose |
|---------|---------|
| `ttc build` | Full pipeline: parse, check, analyse, desugar, emit, format |
| `ttc check` | Parse and type-check only — no emission. For CI use |
| `ttc fmt` | Format `.tt` source files in place |
| `ttc lsp` | Run as a Language Server on stdio |
| `ttc init` | Scaffold a new project: `typhon.toml`, `src/`, `tests/` |
| `ttc trace` | Map a Python traceback back to Typhon source via `.py.map` files |
| `ttc profile` | Instrument emitted code for hot-function detection (opt-in) |

## Language Features (Roadmap)

### Phase 0 — Foundation ✅
- Cargo workspace skeleton with `crates/` and `vendor/` directories
- `val`/`var` keyword tokens (immutable and mutable bindings)
- `ttc fmt` — parses and validates `.tt` source, normalises whitespace
- `ttc check` — validates syntax and emits miette diagnostics
- `ttc init` — scaffolds new projects with `typhon.toml`

### Phase 1 — Core types (months 3–5)
- Salsa incremental database
- Name resolution, scope construction, `val`/`var` enforcement
- Non-nullable types by default, flow narrowing

### Phase 2 — Class and value features (months 6–8)
- `@dataclass(slots=True)` emission; `model` keyword for Pydantic
- Sealed unions and exhaustive `match`
- `Result[T, E]` and the `?` operator

### Phase 3 — Structural typing (months 9–12)
- Generics with angle-bracket syntax
- `interface` declarations and structural subtyping
- Pipe operator, guards, extension methods
- `.dtt` stub files and `unsafe` blocks

## Configuration (`typhon.toml`)

```toml
[project]
name = "myapp"
version = "0.1.0"
src = "src"
out = "build"

[python]
target = "3.13"
free_threaded = false

[emit]
class_default = "dataclass"  # or "pydantic"
format = true

[strictness]
no_implicit_any = true
unused_import = "error"
exhaustive_match = "error"
```

## Workspace Layout

```
ttc/
├── Cargo.toml                  (workspace root)
├── crates/
│   ├── ttc-syntax/             Typhon lexer/parser (extends rustpython-parser)
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
