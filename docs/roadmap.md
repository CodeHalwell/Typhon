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

## Phase 2 — Class and value features (months 6–8) ✅ complete

- ✅ Class emission as `@dataclass(slots=True)`; the `model` keyword for Pydantic, with `extra='forbid'` injected by default (override via `[emit] model-extra`).
- ✅ Sealed unions and exhaustive `match`. (High-value and mechanically simple — front-loaded.)
- ✅ `Result[T, E]` type, the `?` operator, and `with`-chains (multi-line `with name = expr?, …:` sequencing with an optional `else err:` block).
- ✅ Comptime constants with `env()` lookup. Build fails on missing required env.
- ✅ `tower-lsp-server` backend: `tyc lsp` runs on stdio, publishes diagnostics via the check pipeline on `did_open` / `did_change`, and serves a placeholder hover response. The richer hover (symbol type, doc string, definition link) lands once the resolver exposes a `(file, position)` query.

## Phase 3 — Structural typing and advanced features ✅ complete (subset)

- ✅ **Generics syntax decision locked** (PEP 695). The parser accepts
  `def f[T](x: T)` and `type Vector[T] = ...` directly via `rustpython-parser`;
  the resolver declares type params into the function/class scope and the
  emitter round-trips the `[T]` syntax. Type inference treats `T` as `Any`
  until a proper bidirectional inference engine lands.
- ✅ **Interface declarations** (`interface Name:` → `class Name(Protocol):`)
  with structural conformance check on assignment: a class is assignable to
  an interface only when its member shape covers every required member.
  `isinstance(x, Interface)` is rejected unless the interface opts in via
  `@runtime_checkable`. Recursion / signature-compatibility refinement is
  deferred to Phase 4+.
- ✅ **`unsafe`** block keyword — lowers to `if True:` so scoping survives
  the Python round-trip. The type checker tracks `Checker::unsafe_depth`
  and suppresses type-mismatch / nullable-use / interface-isinstance /
  wrong-arg-count / not-callable / non-exhaustive-match diagnostics inside
  the block so users can interface with untyped Python without fighting the
  checker. Errors on lines outside the block are unaffected.
- ✅ **Pure-function detection** with the six-condition rule (sync, no `raise`,
  no `try`, no I/O builtins, no entropy/clocks, no writes to module-level
  `var` state). `@pure`, `@memo`, and `@pure(memo=True)` decorators trigger
  the check; violations are hard errors. Memoised functions get
  `@functools.cache` injected at desugar time; `@pure`/`@memo` markers are
  stripped because they are not real Python names.
  `[strictness] auto-memoise = true` opts every passable function in.
- ✅ **`gather`** block lowers to `asyncio.TaskGroup` by default (cancels
  siblings on first failure). `gather(strategy="best-effort"):` lowers to
  `asyncio.gather(..., return_exceptions=True)`. `import asyncio` is
  injected by the desugar pass when the lowered code references it.
- ✅ **`go`** lowered through `typhon_runtime.tasks.spawn(...)` with a
  strong-ref task registry (never a bare `asyncio.create_task`).
  `go f(x) -> fut` binds the task handle.
- ✅ **Lazy imports** — `lazy import np = numpy` lowers to a thread-safe
  proxy class generated inline by `expand_lazy_imports` (double-checked
  locking, no runtime helper dependency). `lazy from x import …` is rejected
  because it defeats deferral. Module-level `lazy val NAME: T = expr` and
  instance-level `cached_property` are deferred — the syntax pipeline does
  not yet recognise them.
- ✅ **Pipe operator** `a |> f |> g(arg)` lowered to `g(f(a), arg)` left-
  associatively. Guards in `match` cases pass through to Python directly.
- ✅ **`extend`** keyword for adding methods to user-defined classes — an
  alias for `impl` in v1; extension on built-in types is deferred until a
  type-aware call-site rewriter lands.
- ✅ **`.dty` stub files** with `.pyi` interop emission — every `.dty` next to
  the project is compiled to a PEP 561 `.pyi` (function/method bodies become
  `...`, plain `Assign` is dropped, annotated fields are kept). `tyc check
  --stubs` validates that every `.dty` parses, resolves, and type-checks;
  the full mypy-`stubtest` runtime diff is deferred.

At the end of Phase 3, Typhon is useful for a real backend or CLI project.
Everything beyond is polish and ambition.

## Phase 4+ — Beyond v1

- ✅ **Automatic `asyncio.gather` inference** (conservative). Runs of two or
  more independent `name = await callee(...)` statements inside an
  `async def` are folded into an `asyncio.TaskGroup` block when the callee
  is a same-module `async def` and the awaits are statically independent.
  Opt-in via `[strictness] auto-gather = true`. The desugar pass injects
  `import asyncio` if missing.
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
