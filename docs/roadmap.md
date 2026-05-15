# Roadmap

> Excerpted from the [long-term plan](long-term-plan.md). The plan is the source of truth.

Realistic milestones for one person plus AI assistance. The headline target is a useful subset shippable in twelve months.

## Phase 0 — Foundation (months 1–2) ✅ in progress

- Fork `ruff_python_parser` and `ruff_python_ast` into `vendor/`.
- Add one or two custom tokens (`val`, `var`) to confirm the fork-extend workflow.
- Round-trip Python through the fork via `ruff_python_codegen`: parse → emit, verify byte-identical (modulo whitespace) on a corpus of real Python files.
- `clap`-based `ttc` shell with `ttc fmt` working as the simplest end-to-end command.
- `miette` + `thiserror` diagnostic infrastructure.

## Phase 1 — Core types (months 3–5)

- Salsa db with `parse` and `resolve` queries.
- Name resolution and scope construction; `val`/`var` enforcement (no types yet).
- Nominal types: function signatures, assignment compatibility, primitive types, classes.
- Non-nullable by default with flow narrowing on guards and `isinstance` checks.
- `ttc check` produces useful "unknown name" and "type mismatch" diagnostics via `miette`.

## Phase 2 — Class and value features (months 6–8)

- Class emission as `@dataclass(slots=True)`; the `model` keyword for Pydantic.
- Sealed unions and exhaustive `match`. (High-value and mechanically simple — front-load it.)
- `Result[T, E]` type and the `?` operator; `with`-chains.
- Comptime constants with `env()` lookup. Build fails on missing required env.
- `tower-lsp-server` backend: diagnostics and hover working in VS Code.

## Phase 3 — Structural typing and advanced features (months 9–12)

- Generics (angle-bracket syntax, bidirectional inference, type erasure at emit).
- Interface declarations and structural subtyping with memoised relation cache.
- Pure-function detection (conservative syntactic check) and `@functools.cache` emission.
- Explicit `gather` block for `asyncio.gather`.
- Lazy imports and `lazy val`.
- Pipe operator, guards, extension methods.
- `.dtt` stub files and `unsafe` blocks for untyped library interop.

At the end of Phase 3 — roughly month twelve — Typhon is useful for a real backend or CLI project. Everything beyond is polish and ambition.

## Phase 4+ — Beyond v1

- Automatic `asyncio.gather` inference (conservative, `@pure` straight-line code only).
- Loop parallelisation for pure comprehensions on free-threaded Python.
- Richer comptime: `comptime` functions, types as values.
- PGO via `ttc profile`.
- LSP completions and code actions; go-to-definition across `.tt` and `.py` boundaries via source maps.
- Migration tooling from typed `.py` to `.tt` (`Optional[T]` → `T?`, dataclasses → Typhon classes, etc.).

## Scope-cutting rule

The minimum-viable Typhon is **non-null types + sealed unions + `Result` + dataclass emit**. That alone is publishable. Everything else can be sacrificed to ship.

## Concrete next steps

In order:

1. Set up the Cargo workspace skeleton with `crates/` and `vendor/` directories.
2. Get parse → emit round-tripping a real Python file (e.g. one of Django's management commands) without losing anything.
3. Add `val` and `var` as new keyword tokens. Confirm the fork-extend workflow is sustainable.
4. Wire up `clap` with `ttc fmt` as the first working command.
5. Add `miette` for diagnostics. Now any future error has somewhere good-looking to go.

Roughly two months of work. Everything in the plan unfolds from those steps.
