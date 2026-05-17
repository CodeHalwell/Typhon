# Architecture

> Excerpted from the [long-term plan](long-term-plan.md). The plan is the source of truth.

The `tyc` binary is a multi-stage compiler with an embedded LSP, structured as a Cargo workspace of small crates that mirror the pipeline stages. Each stage produces a typed Rust value that the next stage consumes; analysis results are stored as Salsa queries so the LSP can reuse them incrementally.

## Pipeline

```
.ty source files
        │
        ▼
[tyc-syntax]   →  Typhon AST (Python AST + Typhon nodes)
        │
        ▼
[tyc-resolve]  →  symbol tables, scopes, let/mut classification
        │
        ▼
[tyc-types]    →  typed AST, structural subtyping, sealed unions
        │
        ▼
[tyc-analyse]  →  purity, async/concurrency, comptime, optimisation hints
        │
        ▼
[tyc-desugar]  →  plain Python AST
        │
        ▼
[tyc-emit]     →  .py source via ruff_python_codegen + ruff_python_formatter
        │
        ▼
[tyc-lsp]      →  reuses the above stages incrementally via Salsa
```

## Workspace layout

```
tyc/
├── Cargo.toml                  (workspace root)
├── crates/
│   ├── tyc-syntax/             forked ruff_python_ast + parser, Typhon nodes
│   ├── tyc-db/                 Salsa database, input/tracked queries
│   ├── tyc-resolve/            name resolution, imports, let/mut
│   ├── tyc-types/              structural + nominal type checker
│   ├── tyc-analyse/            purity, async-gather, comptime, DCE
│   ├── tyc-desugar/            Typhon AST → Python AST lowering
│   ├── tyc-emit/               Python codegen via vendored ruff_python_codegen
│   ├── tyc-format/             post-process emitter output through ruff format
│   ├── tyc-diagnostics/        miette-based diagnostic rendering
│   ├── tyc-lsp/                tower-lsp-server Backend over tyc-db
│   └── tyc/                    thin CLI binary, clap subcommands
└── vendor/
    ├── ruff_python_ast/        forked from Ruff monorepo
    ├── ruff_python_parser/     forked and extended with Typhon tokens
    └── ruff_python_codegen/    forked for emission
```

This is the same crate-per-stage layout used by `oxc` and `rust-analyzer`. The single most important meta-rule: every external crate gets wrapped behind a one-function-wide module of our own, so when Salsa changes its API or Ruff renames a node, the blast radius stays small.

## Toolchain decisions

| Stage | Primary choice | Fallback | Why |
|-------|----------------|----------|-----|
| Parser | Fork `ruff_python_parser` | `rustpython-parser` | Fastest, most spec-compliant Python parser in Rust. Not on crates.io, so vendor it. |
| AST | Fork `ruff_python_ast` | Hand-written | AST is partly TOML-generated; adding Typhon variants is mechanical. |
| Incremental engine | `salsa` (salsa-rs) | Hand-rolled query cache | Powers `rust-analyzer` and `ty`. Free cancellation and parallel queries. |
| Type checker | Custom on Salsa, `ty` as reference | Embed `ty` as a library | Typhon-specific rules need own checker; `ty` handles the Python subset. |
| Code emission | Fork `ruff_python_codegen` | Hand-written pretty-printer | Small, internal, vendor-friendly. Post-process through `ruff format`. |
| LSP transport | `tower-lsp-server` | `lsp-server` | Ergonomic, active fork on `lsp-types` 0.97+. |
| CLI | `clap` v4 derive | — | Standard. |
| Diagnostics | `miette` + `thiserror` | `ariadne` | Best-in-class source-span rendering. |
| Config file | `serde` + `toml` | — | `typhon.toml`. |
| Arena allocator | `bumpalo` | Stock `Vec`/`Box` | Defer until profiling justifies it. |

## Why vendor the parser

Python's significant-whitespace lexing is non-trivial. Hand-writing a full Python parser using `chumsky` or `lalrpop` would mean spending months catching up to mainstream Python syntax before writing a single new feature. Vendoring Ruff's parser inherits its battle-testing on real codebases and its same-AST contract with `ty`, which simplifies later type-checker integration. The cost is grammar-sync work whenever Python releases new syntax.

## Type checker depth and approach

### Hybrid strategy

1. Run Typhon-specific checks on the Typhon AST: non-nullability, sealed-union exhaustiveness, `Result`/`?` propagation, `let`/`mut`, no-implicit-`Any`, extension-method resolution.
2. Desugar to Python AST with rich type annotations preserved.
3. Optionally run `ty` (as a library) over the desugared AST to catch standard Python typing violations.

### Structural subtyping

TypeScript's `tsc` is the reference: polynomial-time decision algorithm with a memoised relation cache, recursive-type handling via assumed-subtype sets, and detailed diagnostics tracking which exact member is missing or incompatible.

### Salsa queries

Express each analysis as a Salsa query: `parse(file)`, `resolve(module)`, `infer(function)`, `check(module)`. Salsa builds the dependency graph behind the scenes and recomputes only invalidated nodes when a file changes. Durability levels distinguish stdlib queries from user-file queries.

## Code emission

Pipeline: Typhon AST → desugar to plain Python AST → `ruff_python_codegen` → `ruff_python_formatter`. Emitted files carry a generated-header comment.

Source maps mapping `.py` line/column back to `.ty` are written as a sidecar `.py.map` file. The LSP uses these for go-to-definition across the boundary; `tyc trace` maps Python tracebacks back to Typhon source.

There is deliberately **no Typhon-specific runtime package** the user must install. The handful of helpers needed (`Result`/`Ok`/`Err`, `lazy_import`, `str_to_slug`-style extension shims) are emitted inline into each project as a generated `typhon_runtime/` module the build owns.
