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
[tyc-emit]     →  .py source via hand-written printer (tracks .py.map offsets)
        │
        ▼
[tyc-format]   →  in-process whitespace pass + ruff format wrap (when on PATH)
        │
        ▼
[tyc-lsp]      →  reuses the above stages incrementally via Salsa

Parallel surface:
[tyc-vm]       →  walks the parsed Typhon AST directly (default for `tyc run`)
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
│   ├── tyc-emit/               Python codegen (hand-written printer; tracks line offsets for .py.map)
│   ├── tyc-format/             post-process emitter output through ruff format
│   ├── tyc-diagnostics/        miette-based diagnostic rendering
│   ├── tyc-lsp/                tower-lsp-server Backend over tyc-db
│   ├── tyc-vm/                 tree-walking interpreter (tyc run default)
│   ├── tyc-venv/               venv signature introspection → ModuleShapes (shared by CLI + LSP)
│   └── tyc/                    thin CLI binary, clap subcommands
└── vendor/                     Typhon's in-tree fork of Ruff (pinned via vendor/UPSTREAM)
    ├── ruff_text_size/         TextSize / TextRange newtypes
    ├── ruff_source_file/       Line-index over a source string
    ├── ruff_python_trivia/     Whitespace + comment helpers
    ├── ruff_python_ast/        Python AST + Typhon's Mutability extension
    └── ruff_python_parser/     Lexer + parser, plus let/mut soft-keyword support
```

> The Phase-0 plan called for vendoring `ruff_python_codegen` as well. That
> task was deferred: `tyc-emit` currently uses a hand-written printer because
> upstream codegen does not expose the per-statement line-offset hook required
> for `.py.map` source maps. See `tyc/vendor/README.md` for the open follow-up.

This is the same crate-per-stage layout used by `oxc` and `rust-analyzer`. The single most important meta-rule: every external crate gets wrapped behind a one-function-wide module of our own, so when Salsa changes its API or Ruff renames a node, the blast radius stays small.

## Toolchain decisions

| Stage | Primary choice | Fallback | Why |
|-------|----------------|----------|-----|
| Parser | Fork `ruff_python_parser` | `rustpython-parser` | Fastest, most spec-compliant Python parser in Rust. Not on crates.io, so vendor it. |
| AST | Fork `ruff_python_ast` | Hand-written | AST is partly TOML-generated; adding Typhon variants is mechanical. |
| Incremental engine | `salsa` (salsa-rs) | Hand-rolled query cache | Powers `rust-analyzer` and `ty`. Free cancellation and parallel queries. |
| Type checker | Custom on Salsa, `ty` as reference | Embed `ty` as a library | Typhon-specific rules need own checker; `ty` handles the Python subset. |
| Code emission | Hand-written pretty-printer (today) | Fork `ruff_python_codegen` (deferred) | Hand-written printer tracks line offsets for `.py.map`; upstream codegen lacks that hook. Post-process through `ruff format`. |
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

Pipeline: Typhon AST → desugar to plain Python AST → `tyc-emit` hand-written printer → `ruff format` post-process (when `[emit] format = true`). Emitted files carry a generated-header comment.

Source maps mapping `.py` lines back to `.ty` are written as a sidecar `.py.map` file. The v2 format stores an `(out_line → ty_line)` table, built by composing **two** maps:

1. `tyc-emit`'s printer records a `line_offsets` table as it prints each statement, giving `out_line → preprocessed_line`.
2. Each line-count-changing preprocessor pass returns its own `preprocessed_line → input_line` table from a `*_mapped` variant (`expand_pipes_mapped`, `expand_gather_blocks_mapped`, …); `tyc build` folds them with `compose_line_maps` to get `preprocessed_line → ty_line`.

Both halves are required. Before v1.0.0-alpha.7 only the first existed and its values were written out directly, so the table named lines of the preprocessed buffer — which for any file using `?`, `gather:`, a `with`-chain, `rescue`, pipes or typed unpack is a different, longer file than the one the user wrote, frequently past its EOF.

The plain entry points (`expand_pipes(s) -> String`) are thin wrappers over the mapped variants, so a consumer that does not need provenance — the VM, `tyc check`, the REPL — is unaffected.

The composed table is **not** monotonic in general: a `with`-chain copies its `else` body into each binding's guard, so those output lines point back to a line above their neighbours. Do not assume monotonicity when consuming it.

`tyc trace` uses the map to rewrite Python tracebacks back to Typhon source, `tyc debug --break` to place breakpoints, and the LSP for cross-file go-to-definition across the `.ty`/`.py` boundary.

There is deliberately **no Typhon-specific runtime package** the user must install. The handful of helpers needed (`Result`/`Ok`/`Err`, `lazy_import`, `str_to_slug`-style extension shims) are emitted inline into each project as a generated `typhon_runtime/` module the build owns.
