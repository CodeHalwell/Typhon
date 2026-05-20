# Roadmap

> Excerpted from the [long-term plan](long-term-plan.md). The plan is the source of truth.

Realistic milestones for one person plus AI assistance. The headline target is a useful subset shippable in twelve months.

## Phase 0 — Foundation (months 1–2) ✅ complete

- ✅ Fork `ruff_python_parser` and `ruff_python_ast` into `vendor/`. The
  vendored crates are active members of the Cargo workspace; all consumer
  crates use `ruff_python_ast` via `tyc_syntax::parse_module`. The migration
  off `rustpython-parser` is complete — see `tyc/vendor/README.md` for
  migration details.
- ✅ Add one or two custom tokens (`let`, `mut`) to confirm the fork-extend workflow.
- ✅ Round-trip Python through the fork: `tyc-emit`'s hand-written printer
  covers the Python subset used in every built and tested Typhon module;
  round-trip correctness is verified by the integration test suite
  (`cargo test --workspace`). A corpus sweep over third-party Python files is
  a future hardening task, not a blocker.
- ✅ `clap`-based `tyc` shell with `tyc fmt` working as the simplest end-to-end command.
- ✅ `miette` + `thiserror` diagnostic infrastructure.

## Phase 1 — Core types (months 3–5) ✅ complete

- Salsa db with cached `preprocessed_text` and `module_decl_names` queries; richer queries unlock as their outputs become `salsa::Update`-friendly.
- Name resolution and scope construction with module / function / class / comprehension scopes.
- `let` / `mut` enforcement: reassigning a `let` is a hard error; top-level bindings default to `let`.
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
  `def f[T](x: T)` and `type Vector[T] = ...` directly via the vendored Ruff
  parser; the resolver declares type params into the function/class scope and
  the emitter round-trips the `[T]` syntax. Bidirectional inference binds
  typevars from actual arguments (recursively, with conflict-widening) and
  substitutes them in the return type; bounded-type-var checking is in place.
  Full variance and higher-kinded forms are deferred.
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
  `mut` state). `@pure`, `@memo`, and `@pure(memo=True)` decorators trigger
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
  because it defeats deferral. Module-level `lazy let NAME: T = expr` lowers
  to a sentinel-cached `lazy_let(lambda: expr)`; class-body `lazy let` lowers
  to `@cached_property`. Both round-trip through `tyc fmt`.
- ✅ **Pipe operator** `a |> f |> g(arg)` lowered to `g(f(a), arg)` left-
  associatively. Guards in `match` cases pass through to Python directly.
- ✅ **`extend`** keyword for adding methods to user-defined classes
  (alias for `impl`) and for the recognised Python built-ins (`str`,
  `list`, `dict`, …). Built-in extensions are extracted to module-level
  free functions `__typhon_ext_<TYPE>__<METHOD>` at desugar time, and
  call sites are rewritten when the receiver has a matching static
  annotation. No monkey-patching of built-ins.
- ✅ **`.dty` stub files** with `.pyi` interop emission — every `.dty` next to
  the project is compiled to a PEP 561 `.pyi` (function/method bodies become
  `...`, plain `Assign` is dropped, annotated fields are kept). `tyc check
  --stubs` parses every `.dty` and diffs its surface API (functions, classes,
  methods, annotated fields, parameter shapes) against the sibling `.ty`/`.py`
  implementation, emitting `tyc::stub_mismatch` diagnostics for
  missing-in-impl / missing-in-stub / signature-mismatch findings. A runtime
  introspection probe (mypy's `stubtest` proper) is still a follow-up.

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
- ✅ **PGO via `tyc profile`**. When `[strictness] pgo-memoise = true`,
  `tyc build` loads `typhon-profile.json` from the project root and
  promotes every `@pure` function whose observed call count meets
  `pgo-min-calls` (default 100) to `@functools.cache`, even when the
  user did not write `@memo`. Complements `auto-memoise` (which caches
  every pure function regardless of profile data). Missing profile
  file is not an error — PGO is best-effort.
- ✅ **LSP completions and code actions**. `textDocument/completion`
  returns visible bindings (walking the cursor's enclosing scope chain),
  Typhon keywords (`let`, `mut`, `gather`, `go`, `lazy`, …), and a
  small set of common Python builtins; the LSP client filters by prefix.
  `textDocument/codeAction` offers a "Remove unused import" quick-fix
  for every `tyc::unused_import` diagnostic in range. Cross-file
  go-to-definition across `.ty` / `.py` boundaries via source maps is
  still pending the v2 source-map format.
- Migration tooling from typed `.py` to `.ty` (`Optional[T]` → `T?`, dataclasses → Typhon classes, etc.).
- **`ty` integration** as a complementary second-stage checker over the
  desugared Python. Planned in two phases: first as a subprocess
  invocation of `ty check` with diagnostic attribution via the source
  maps (no dependency on the Ruff vendor), later as an embedded
  library sharing the Salsa db. See [docs/ty-integration.md](ty-integration.md)
  for the full plan.

## Scope-cutting rule

The minimum-viable Typhon is **non-null types + sealed unions + `Result` + dataclass emit**. That alone is publishable. Everything else can be sacrificed to ship.

## Concrete next steps

Phases 0–3 are complete. The current frontier is Phase 4+:

1. Corpus round-trip sweep: run `tyc build` over a representative set of
   third-party Python projects and compare the emitted `.py` against the
   source semantically. Not a blocker (the test suite is green), but
   hardens confidence.
2. Promote `bind_typevars_and_substitute` into a proper structural
   sub-type checker that handles variance and bounded higher-kinded forms.
3. Expand the Salsa boundary: make `resolve_module` and `check_module` into
   Salsa-tracked queries so the LSP second-check latency drops to near-zero
   for unchanged files.
4. Loop parallelisation for pure comprehensions on free-threaded Python.
5. Runtime `stubtest` probe via `mypy --stubtest` as a complement to the
   AST-level `tyc check --stubs` diff.
