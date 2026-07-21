# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this repository is

Typhon is **a statically-typed, stricter superset of Python that compiles to clean, idiomatic CPython 3.13+** with zero runtime dependency on the toolchain. The compiler, language server, formatter, debugger wrapper, REPL, and a tree-walking interpreter are all one Rust binary, `tyc`. Every `.ty` file emits valid `.py`; not all `.py` is valid Typhon.

The repo has two distinct surfaces, which need different workflows:

1. **The Rust compiler** under `tyc/` — a Cargo workspace of crates. This is where almost all source-code work happens.
2. **The Typhon language itself** — `.ty` source in `examples/`, `stress/`, project scaffolds, plus the docs that define the language.

> **Before reading or writing any `.ty` / `.dty` / `typhon.toml`, or touching the `tyc/crates/`, invoke the bundled `typhon` skill** (`.claude/skills/typhon/`). It is the authoritative field reference for the language, the compiler pipeline, the VM, the generated runtime, and every `tyc::` diagnostic. LLMs have no prior knowledge of Typhon — **trust the docs and the compiler over Python assumptions.** When the skill and a doc disagree, the doc wins; when docs and the compiler disagree, the compiler wins (verify with `tyc check`).

Current release: **v1.0.0-alpha.6** (workspace version in `tyc/Cargo.toml`). The language is **additive across the v0.3.0 → v1.0.0-alpha line** — every previously-accepted program continues to type-check identically. **v1.0.0-alpha.2** was the first deliberate exception: a few conservative new diagnostics (`tyc::not_a_context_manager`, `tyc::raise_non_exception`, `tyc::frozen_inheritance_conflict`) reject programs that were already guaranteed to crash at runtime. **v1.0.0-alpha.3** continues in the same spirit — it adds no new syntax and closes four flow-narrowing invalidation holes as conservative widenings that only reject code relying on a previously-*unsound* narrowing. **v1.0.0-alpha.4** does the same for the H5 scope-blind class-unification hole: a local class no longer unifies with a same-named foreign class of a provably different shape. **v1.0.0-alpha.5** is a performance release — VM Tier 1 (allocation-light small ints, method-call/dispatch caching, slot-resolved locals), an `[optimise]` config profile, a new `tyc::perf_*` advice-lint family, a free-threading parallelisation wave, and native PEP 810 lazy imports on 3.15 targets — every change opt-in or advice-only, so no previously-*correct* program changes behaviour. **v1.0.0-alpha.6** is a maintenance release — the July dependency wave carried safely across the `toml` 0.8 → 1.x major (fixing the venv-introspection allow-list regression that bump introduced before it shipped), six more secret-name keywords in the now-shared `tyc::contains_secret_literal` table (warn-level only), release-pipeline artifact-action re-pinning, and docs/repo hygiene — no language change. Treat "additive on *correct* programs" as a hard compatibility constraint when changing the type checker — a new diagnostic must only ever fire on code that already failed at runtime, never narrow a program that ran correctly.

## Building, testing, linting (the Rust compiler)

All cargo commands run from the **`tyc/`** directory (the workspace root). The toolchain is pinned to Rust **1.94** via `tyc/rust-toolchain.toml`.

```bash
cd tyc

cargo build --release            # builds the tyc binary → tyc/target/release/tyc
cargo test --workspace           # full test suite
cargo fmt -- --check             # formatting gate (CI runs this)
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

**CI treats all warnings as errors** (`RUSTFLAGS: -D warnings`). Code that compiles locally with warnings will fail CI — keep the tree warning-clean. CI runs three jobs: `test` (fmt check → clippy → `cargo test`), `security` (`cargo-deny` over `tyc/Cargo.toml` — advisories, licences, source/registry bans per `tyc/deny.toml`), and `perf-gate`.

Run a single test:

```bash
cargo test --workspace pipeline                 # by name substring
cargo test -p tyc-types                          # one crate
cargo test -p tyc --test pipeline                # one integration test file
```

Integration tests for the CLI/pipeline live in `tyc/crates/tyc/tests/` (`pipeline.rs`, `build_features.rs`); most behaviour is covered by per-crate unit tests.

**Performance gate:** `scripts/perf-gate.sh` (run from repo root) times the full `tyc build` pipeline over a fixed, network-free corpus and fails if the median regresses >20% past `perf-baseline.json`. It needs a release `tyc` binary. Use `scripts/perf-gate.sh --update` to refresh the baseline after an intentional change; methodology is in `docs/performance-baseline.md`.

## Working in the Typhon language (the `.ty` workflow)

Use the built `tyc` binary (alias it: `alias tyc="$PWD/tyc/target/release/tyc"`). Core loop:

```bash
tyc init <name>      # scaffold typhon.toml + src/ + tests/
tyc fmt src/         # whitespace-normalise, then ruff format wrap when ruff is on PATH
tyc check src/       # parse + resolve + type-check, no artifacts (use this in CI)
tyc run              # execute via the in-process tree-walking VM (no .py written, no CPython spawned)
tyc build            # full pipeline → build/main.py + build/.sourcemaps/*.py.map; bootstraps pyproject.toml + uv sync
tyc explain <code>   # diagnostic catalog entry for a tyc:: code (offline)
tyc cheatsheet       # 30-second syntax table
```

`tyc run` defaults to the VM; `tyc run --compile` falls back to build-then-exec. Full subcommand reference: `docs/cli.md` and the skill's `CLI.md`.

## Compiler architecture (the big picture)

`tyc` is a multi-stage compiler with an embedded LSP, structured so each crate mirrors one pipeline stage. Each stage produces a typed Rust value the next stage consumes; analysis results are stored as **Salsa** queries so the LSP can reuse them incrementally. Full detail: `docs/architecture.md`.

```
.ty source
  → tyc-syntax    Typhon AST (forked Ruff Python AST + Typhon nodes; preprocesses let/mut, enum, as!, etc.)
  → tyc-resolve   symbol tables, scopes, let/mut classification, import resolution
  → tyc-types     typed AST: structural subtyping, sealed unions, non-nullability, generics (PEP 695)
  → tyc-analyse   purity, async/concurrency (gather/go), comptime, optimisation hints, effect lints
  → tyc-desugar   Typhon AST → plain Python AST
  → tyc-emit      .py via a hand-written printer (records line offsets for .py.map source maps)
  → tyc-format    in-process whitespace pass + `ruff format` wrap (when ruff is on PATH)
  → tyc-lsp       tower-lsp Backend reusing the above stages incrementally via Salsa

Parallel surface:
    tyc-vm         tree-walking interpreter over the parsed Typhon AST (default for `tyc run`)
```

Supporting crates: **`tyc-db`** (Salsa database, input/tracked queries), **`tyc-diagnostics`** (miette-based rendering), **`tyc-venv`** (venv signature introspection → `ModuleShapes`, shared by CLI + LSP for third-party type-checking), and **`tyc`** (the thin clap CLI binary). The vendored Ruff fork lives under `tyc/vendor/` (`ruff_python_parser`, `ruff_python_ast`, etc.), pinned via `vendor/UPSTREAM` — see `tyc/vendor/README.md` for the fork rationale (Python's significant-whitespace lexing is non-trivial; the fork inherits Ruff's battle-testing and a same-AST contract with Astral's `ty`).

Key architectural facts that aren't obvious from any single file:

- **Wrap every external crate behind a one-function-wide module of our own.** This is the project's single most important meta-rule: when Salsa changes its API or Ruff renames a node, the blast radius stays small. Follow it when adding dependencies.
- **No Typhon runtime package is installed in production.** The handful of helpers needed (`Result`/`Ok`/`Err`, `lazy_import`, `deep_freeze`, `checked_cast`, task spawning, extension shims) are emitted inline into each project as a generated `typhon_runtime/` module that the build owns. See the skill's `RUNTIME.md`.
- **Source maps are first-class.** `tyc-emit` uses a hand-written printer (not upstream Ruff codegen) specifically because it must record a per-statement `(out_line → ty_line)` table for the `.py.map` v2 sidecars. `tyc trace`/`tyc debug` rewrite Python tracebacks/breakpoints back to `.ty`, and the LSP uses the maps for cross-`.ty`/`.py` go-to-definition. Preserve this when touching emission.
- **The VM is a parallel execution surface, not a stage in the build pipeline.** It walks the Typhon AST directly. It must stay a drop-in for `tyc build && python` — VM/CPython divergences are bugs. The VM uses arbitrary-precision integers (`num_bigint`) and CPython-matching value semantics (dataclass equality/repr/hashing, set equality, float repr).
- **Type-checking is hybrid.** Typhon-specific checks run on the Typhon AST; the desugared Python can optionally be sent to Astral's `ty` (`[checker] external = "ty"` or `--with-ty`) to cover stdlib/C-extension APIs introspection can't see. `ty` diagnostics are re-attributed to `.ty` source.

## Conventions and constraints

- **Docs are the source of truth for the language.** `docs/long-term-plan.md` is canonical for design decisions; `docs/language.md`, `docs/cli.md`, `docs/configuration.md`, `docs/vm.md`, `docs/architecture.md`, and `docs/diagnostics/*.md` are focused references. The published site under `docs-site/` (Astro + Starlight) is self-contained and **not** generated from `docs/` — update both when changing user-facing behaviour.
- **Every release is logged.** `CHANGELOG.md` records every release back to v0.1.0. A behaviour or surface change should land with a changelog entry.
- **Each `tyc::` diagnostic has a doc page.** A new diagnostic needs a `docs/diagnostics/<code>.md` page (surfaced by `tyc explain` and linked from the diagnostic's `url(...)`).
- **Language changes must be additive** on the accepted surface (see above). New forms are fine; breaking a previously-accepted program is not.
- **The example/stress corpus is the regression net.** `examples/` (curated single-file exercises + `examples/apps/` multi-file projects) and `stress/` are compile-clean against the current release; new type-checker work is expected to keep them green and false-positive-free.
- **The open type-system frontier** (embedded `ty` Phase 2, accumulator-loop parallelisation, etc.) is tracked in `TYPE_SYSTEM_FRONTIER.md`.

## Git workflow for this environment

Develop on a task-specific branch (e.g., claude/feature-name); create it locally if missing. Commit with clear messages and push with git push -u origin <branch>. Do not open a pull request unless explicitly asked. CI runs on main, dev/**, and claude/** pushes.
