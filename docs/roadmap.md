# Roadmap

> Excerpted from the [long-term plan](long-term-plan.md). The plan is the source of truth.

Realistic milestones for one person plus AI assistance. The headline target is a useful subset shippable in twelve months.

## Phase 0 — Foundation (months 1–2) — substantially complete

- ☐ Fork `ruff_python_parser` and `ruff_python_ast` into `vendor/`. *Deferred — currently using `rustpython-parser` 0.4 from crates.io as the Phase-0 fallback. The fork lands in a follow-up.*
- ✅ Add one or two custom tokens (`val`, `var`) to confirm the fork-extend workflow.
- ☐ Round-trip Python through the fork via `ruff_python_codegen`: parse → emit, verify byte-identical (modulo whitespace) on a corpus of real Python files. *Hand-written `tyc-emit` printer covers the Python subset used in Phase 0/1 round-trip tests; corpus verification deferred until the ruff fork lands.*
- ✅ `clap`-based `tyc` shell with `tyc fmt` working as the simplest end-to-end command.
- ✅ `miette` + `thiserror` diagnostic infrastructure.

## Phase 1 — Core types (months 3–5) ✅ complete

- Salsa db with cached `preprocessed_text` and `module_decl_names` queries; richer queries unlock as their outputs become `salsa::Update`-friendly.
- Name resolution and scope construction with module / function / class / comprehension scopes.
- `val` / `var` enforcement: reassigning a `val` is a hard error; top-level bindings default to `val`.
- Nominal types: function signatures, assignment compatibility, primitive types, classes, generic containers.
- Non-nullable by default with flow narrowing on `is None`, `is not None`, and `isinstance(x, T)` checks. `T?` is sugar for `T | None`.
- `tyc check` emits useful "unknown name", "type mismatch", "nullable use", and "wrong argument count" diagnostics via `miette`.

## Phase 2 — Class and value features (months 6–8) — substantially complete

- ✅ Class emission as `@dataclass(slots=True)`; the `model` keyword for Pydantic, with `extra='forbid'` injected by default (override via `[emit] model-extra`).
- ✅ Sealed unions and exhaustive `match`. (High-value and mechanically simple — front-loaded.)
- ✅ `Result[T, E]` type, the `?` operator, and `with`-chains (multi-line `with name = expr?, …:` sequencing with an optional `else err:` block).
- ✅ Comptime constants with `env()` lookup. Build fails on missing required env.
- ☐ `tower-lsp-server` backend: diagnostics and hover working in VS Code. The crate is currently a 4-line stub; the `tyc lsp` subcommand prints a "not yet implemented" notice.

## Phase 3 — Structural typing and advanced features (months 9–12)

- **Generics syntax decision locked** (angle brackets vs PEP 695). See *Open questions* in [long-term-plan.md](long-term-plan.md). Implementation follows the decision.
- Generics: bidirectional inference, type erasure at emit.
- Interface declarations and structural subtyping with memoised relation cache. `is`/`isinstance` against an interface is rejected unless explicitly opted into — `@runtime_checkable` only validates attribute presence, not signatures.
- `unsafe` block semantics: lexical regions with an `Unsafe[T]` boundary marker the checker enforces at every region boundary.
- Pure-function detection bound to the six-condition rule (sync, hashable args, no I/O, no entropy/clocks, no mutable module state, no exceptions). `@functools.cache` / `lru_cache` emission **only** under explicit opt-in (`@memo`, `@pure(memo=True)`, or `[strictness] auto-memoise = true`).
- `gather` block lowered to `asyncio.TaskGroup` by default (cancels siblings on first failure). `gather(strategy="best-effort"):` for `asyncio.gather(..., return_exceptions=True)`.
- `go` lowered through `typhon_runtime.tasks.spawn` with a strong-ref registry — never to a bare `asyncio.create_task`.
- Lazy imports — `lazy import np = numpy` only; `lazy from x import a, b` is rejected because it defeats deferral. Module-level `lazy val` uses a sentinel + lock helper, instance-level uses `cached_property`.
- ✅ Pipe operator `a |> f |> g(arg)` lowered to `g(f(a), arg)` left-associatively in the preprocessor. Guards in `match` cases pass through to Python directly (no extra desugaring needed). Extension methods still pending.
- `.dty` stub files **and** `.pyi` interop emission; `tyc check --stubs` ports mypy's `stubtest` for drift detection. `unsafe` blocks for untyped library interop.

At the end of Phase 3 — roughly month twelve — Typhon is useful for a real backend or CLI project. Everything beyond is polish and ambition.

## Phase 4+ — Beyond v1

- Automatic `asyncio.gather` inference (conservative, `@pure` straight-line code only).
- Loop parallelisation for pure comprehensions on free-threaded Python.
- Richer comptime: `comptime` functions, types as values.
- PGO via `tyc profile`.
- LSP completions and code actions; go-to-definition across `.ty` and `.py` boundaries via source maps.
- Migration tooling from typed `.py` to `.ty` (`Optional[T]` → `T?`, dataclasses → Typhon classes, etc.).

## Scope-cutting rule

The minimum-viable Typhon is **non-null types + sealed unions + `Result` + dataclass emit**. That alone is publishable. Everything else can be sacrificed to ship.

## Concrete next steps

In order:

1. Set up the Cargo workspace skeleton with `crates/` and `vendor/` directories.
2. Get parse → emit round-tripping a real Python file (e.g. one of Django's management commands) without losing anything.
3. Add `val` and `var` as new keyword tokens. Confirm the fork-extend workflow is sustainable.
4. Wire up `clap` with `tyc fmt` as the first working command.
5. Add `miette` for diagnostics. Now any future error has somewhere good-looking to go.

Roughly two months of work. Everything in the plan unfolds from those steps.
