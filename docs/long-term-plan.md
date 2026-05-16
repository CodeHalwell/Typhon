# Typhon — A Statically-Typed Superset of Python

## Harmonised Implementation Plan

## Executive summary

Typhon is a statically-typed, stricter superset of Python that compiles to clean, readable CPython 3.13+ code with no runtime dependency on the toolchain. Every `.ty` file emits valid, idiomatic `.py`; not all `.py` is valid Typhon. The compiler and language server live in a single Rust binary called `ttc`.

The architecture is a classical multi-stage transpiler: parser, type checker, analyser, desugarer, emitter, plus an embedded LSP. It piggy-backs on the modern Rust-Python ecosystem (Ruff for parsing and codegen, Salsa for incremental computation, ty and Pyrefly as architectural references, Pydantic v2 as an opt-in emit target, free-threaded Python 3.13t/3.14t for the parallelism story).

The risk in the project is not technological. Every individual stage has a mature, MIT-licensed Rust implementation to depend on, vendor, or learn from. The risk is scope. Two areas will dominate effort: structural type checking and inference, and keeping a forked grammar in sync with upstream Python syntax. A useful subset is shippable in roughly twelve months for one person with AI assistance; the full vision is a multi-year project.

## Goals and non-goals

### Goals

- **Static safety**: non-nullable by default, no implicit `Any`, explicit error handling via `Result[T, E]`.
- **Modern ergonomics**: `val`/`var`, interfaces, sealed unions with exhaustive matching, guards, pipes, comptime, lazy loading.
- **Clean compilation** to standard Python with no runtime dependency on Typhon-specific machinery.
- **First-class tooling**: a single `ttc` binary that builds, checks, formats, and runs as an LSP, with sub-100 ms incremental feedback in the editor.
- **Honest interop** with existing Python via `.dty` stubs and explicit `unsafe` regions.

### Non-goals

- **Replacing CPython.** Typhon targets CPython 3.13+ (and the free-threaded build); it is not a new runtime.
- **Beating Pyrefly or ty on typing-spec conformance in v1.** Day-one Typhon supports only the subset of typing-spec features it needs.
- **Aggressive auto-parallelisation in v1.** The risky bits ship behind opt-in keywords first, inference later.
- **A novel package manager.** Standard pip/uv workflows are fine.

## Architecture

The `ttc` binary is a multi-stage compiler with an embedded LSP, structured as a Cargo workspace of small crates that mirror the pipeline stages. Each stage produces a typed Rust value that the next stage consumes; analysis results are stored as Salsa queries so the LSP can reuse them incrementally.

### Pipeline

```
.ty source files
        │
        ▼
[tt-parser]    →  Typhon AST (Python AST + Typhon nodes)
        │
        ▼
[tt-resolve]   →  symbol tables, scopes, val/var classification
        │
        ▼
[tt-checker]   →  typed AST, structural subtyping, sealed unions
        │
        ▼
[tt-analyser]  →  purity, async/concurrency, comptime, optimisation hints
        │
        ▼
[tt-desugar]   →  plain Python AST
        │
        ▼
[tt-emitter]   →  .py source via ruff_python_codegen + ruff_python_formatter
        │
        ▼
[tt-lsp]       →  reuses the above stages incrementally via Salsa
```

### Workspace layout

```
ttc/
├── Cargo.toml                  (workspace root)
├── crates/
│   ├── ttc-syntax/             forked ruff_python_ast + parser, Typhon nodes
│   ├── ttc-db/                 Salsa database, input/tracked queries
│   ├── ttc-resolve/            name resolution, imports, val/var
│   ├── ttc-types/              structural + nominal type checker
│   ├── ttc-analyse/            purity, async-gather, comptime, DCE
│   ├── ttc-desugar/            Typhon AST → Python AST lowering
│   ├── ttc-emit/               Python codegen via vendored ruff_python_codegen
│   ├── ttc-format/             post-process emitter output through ruff format
│   ├── ttc-diagnostics/        miette-based diagnostic rendering
│   ├── ttc-lsp/                tower-lsp-server Backend over ttc-db
│   └── ttc/                    thin CLI binary, clap subcommands
└── vendor/
    ├── ruff_python_ast/        forked from Ruff monorepo
    ├── ruff_python_parser/     forked and extended with Typhon tokens
    └── ruff_python_codegen/    forked for emission
```

This is the same crate-per-stage layout used by `oxc` and `rust-analyzer`. The single most important meta-rule: every external crate gets wrapped behind a one-function-wide module of our own, so when Salsa changes its API or Ruff renames a node, the blast radius stays small.

## Toolchain decisions

These are the load-bearing choices. Each has a sensible fallback if the primary option turns out to be wrong.

| Stage | Primary choice | Fallback | Why |
|-------|----------------|----------|-----|
| Parser | Fork `ruff_python_parser` | `rustpython-parser` (on crates.io) | Ruff is the fastest, most spec-compliant Python parser in Rust. Not on crates.io, so vendor it. |
| AST | Fork `ruff_python_ast` | Hand-written | AST is partly TOML-generated; adding Typhon variants is mechanical. |
| Incremental engine | `salsa` (salsa-rs) | Hand-rolled query cache | Powers `rust-analyzer` and `ty`. Free cancellation and parallel queries. |
| Type checker | Custom on Salsa, `ty` as reference | Embed `ty` as a library on the desugared AST | Typhon-specific rules (non-null, Result, sealed unions) require own checker; `ty` handles the Python subset. |
| Code emission | Fork `ruff_python_codegen` | Hand-written pretty-printer | Small, internal, vendor-friendly. Post-process through `ruff format`. |
| LSP transport | `tower-lsp-server` (community fork) | `lsp-server` (rust-analyzer style) | Ergonomic, active fork on `lsp-types` 0.97+. |
| CLI | `clap` v4 derive | — | Standard. |
| Diagnostics | `miette` + `thiserror` | `ariadne` | Best-in-class source-span rendering. |
| Config file | `serde` + `toml` | — | Standard. `typhon.toml`. |
| Arena allocator | `bumpalo` (if AST grows large) | Stock `Vec`/`Box` | Used by `oxc`; defer until profiling justifies it. |

### Why not start from scratch on the parser

Python's significant-whitespace lexing is non-trivial. Hand-writing a full Python parser using `chumsky` or `lalrpop` would mean spending months catching up to mainstream Python syntax before writing a single new feature. Vendoring Ruff's parser inherits its battle-testing on real codebases and its same-AST contract with `ty`, which simplifies later type-checker integration. The cost is grammar-sync work whenever Python releases new syntax.

## Language design

This is the consolidated feature set. Each subsection covers what the feature does, how it desugars, and what the checker must enforce.

### Type system

#### Non-nullable by default

A plain `T` forbids `None`; `T?` is the optional form. Internally `T?` is represented as a `Nullable[T]` wrapper but emits as `T | None` in Python annotations. The checker uses flow-sensitive analysis to narrow `T?` to `T` inside guards and null-checks. Attempting to call a method on a `T?` without a check is a compile error.

#### Generics

Angle-bracket syntax (`def f<T>(x: T) -> T`) instead of PEP 484 TypeVars. Inference is bidirectional: constraints flow from arguments to type parameters, falling back to explicit annotation when ambiguous. Generics are type-erased at emit time; runtime relies on Python's duck typing and (where present) Pydantic validation.

#### Interfaces (structural)

`interface` declarations are structural contracts, like Python's `typing.Protocol`. The checker verifies that a candidate type provides every required member with compatible signatures, with memoised "assumed subtype" sets to handle recursion. Interfaces emit as `typing.Protocol` subclasses.

#### Sealed unions

`type Shape = Circle | Rectangle | Triangle` declares a finite, sealed sum type. `match` on a sealed union must cover every variant or include a wildcard; the checker compares the set of handled variants against the union's declared members and errors on missing cases. This is the single biggest static-safety win over current Python and is mechanically simple to implement.

#### No implicit `Any`

`Any` is a top type, but its inference is a compile error outside an explicit `unsafe` block. Untyped library calls must be wrapped in `unsafe` or shimmed with a `.dty` stub. This is strictly stricter than TypeScript's `noImplicitAny`.

### Classes and `impl` blocks

`class` declarations are minimalist: no explicit `__init__`, no `self` parameter on methods. Separate `impl` blocks attach methods to a class, in the style of Rust. The desugarer merges `impl` blocks into the class definition and inserts `self`.

Default emit target is `@dataclass(slots=True)`, not `BaseModel`. Pydantic emission is opt-in via the `model` keyword. This avoids forcing a Pydantic runtime dependency on every Typhon class, and keeps validation cost explicit where it matters (boundary types, API models) rather than ubiquitous.

```python
# Typhon
class User:
    id: int
    name: str = "anon"
    email: str?

# Emitted Python (default: dataclass)
from dataclasses import dataclass

@dataclass(slots=True)
class User:
    id: int
    name: str = "anon"
    email: str | None = None
```

```python
# Typhon
model ApiUser:
    id: int
    email: str

# Emitted Python (model keyword: Pydantic)
from pydantic import BaseModel

class ApiUser(BaseModel):
    id: int
    email: str
```

### Error handling

#### `Result[T, E]`

`Result[T, E]` is a sealed sum type with two constructors, `Ok(T)` and `Err(E)`. It emits as a small ADT (a tagged dataclass) defined in a single-file runtime helper that is inlined into output when `Result` is used, so there is no separate package dependency.

#### The `?` operator

The `?` suffix on a `Result`-typed expression unwraps `Ok` and short-circuits `Err` to the enclosing function. The checker enforces that `?` appears only inside a function whose return type is a compatible `Result`. Desugaring is a localised `if isinstance(_x, Err): return _x; v = _x.value` pattern; not a try/except, to keep stack traces clean and avoid surprising exception semantics.

#### `with`-chains

A `with`-chain (modelled on Elixir) sequences several `Result`-producing expressions, binding the success value of each, with a single `else` block that catches the first `Err`. It desugars to a flat sequence of guarded assignments rather than nested try/except.

```python
# Typhon
with user   = db.find_user(id)?,
     perms  = check_perms(user)?,
     report = build_report(user, perms)?:
    return Ok(report)
else err:
    log.warn(err)
    return Err(err)
```

### Async and concurrency

#### Explicit `async`, not inferred

After review, async-by-default with full inference is rejected. Inferring async means a function's "colour" changes when a deep callee changes, which makes refactoring fragile and stack traces confusing. Typhon uses explicit `async def`, matching Python's existing semantics.

What Typhon does add is a static check: a function declared `async` that contains no `await` is a warning (likely a mistake), and a sync function that calls an async one without `await` is a hard error with a clear diagnostic.

#### Automatic `asyncio.gather` (opt-in)

The analyser detects sequences of independent `await` statements and rewrites them as `asyncio.gather`, but only when (a) every called function is marked or inferred `@pure`, (b) the LHS bindings do not alias, and (c) the statements form a straight-line block. This conservative version fires on a small fraction of real code but never introduces bugs. A more aggressive mode is opt-in via an explicit `gather` keyword the user writes themselves.

```python
# Typhon: explicit, always safe
gather:
    user   = fetch_user(id)
    posts  = fetch_posts(id)
    notifs = fetch_notifications(id)

# Emitted Python
user, posts, notifs = await asyncio.gather(
    fetch_user(id),
    fetch_posts(id),
    fetch_notifications(id),
)
```

#### Free-threaded Python

Python 3.13 ships an experimental free-threaded build; 3.14 (Phase II) makes it officially supported with ~5-10% single-thread overhead. When `typhon.toml` sets `free-threaded = true`, the analyser is allowed to emit `ThreadPoolExecutor`-based parallelism for pure-function comprehensions on large collections. The emitter inserts a runtime `sys._is_gil_enabled()` check and falls back to sequential execution if the GIL is present. Default-off until 3.14 is the default Python.

#### `go` spawn

`go f(x)` is sugar for `asyncio.create_task(f(x))` in async contexts and `concurrent.futures.ThreadPoolExecutor.submit` on free-threaded builds for CPU-bound functions. The form `go f(x) -> fut` binds the task handle.

### `val` and `var`

`val` is immutable, `var` is mutable. The checker rejects reassignment to a `val` after its declaration. `var` is preserved as-is but parallelisation passes refuse to touch any binding captured as `var` by a spawned task without explicit synchronisation. Top-level module bindings default to `val` unless declared `var`.

### Lazy loading

`lazy import np = numpy` desugars to a `typhon_runtime.lazy_import_proxy` that defers module loading until first attribute access, built on `importlib.util.LazyLoader`. `lazy val` module-level bindings desugar to a cached getter. `lazy[list[T]]` return types emit generator functions instead of materialised lists.

### Compile-time evaluation (`comptime`)

A `comptime` binding or expression is evaluated by `ttc` at compile time inside a sandboxed interpreter that supports pure arithmetic, string operations, environment-variable lookup via `env(name, default?)`, simple container construction, and calls to other `comptime` functions. The result is inlined as a literal in the emitted Python. Anything outside the allowed set is a compile error.

This is the highest-leverage feature in the spec. Build-time env validation alone is worth shipping. Push it earlier in the roadmap than instinct suggests.

```python
# Typhon
comptime val PORT: int = int(env("PORT", "8080"))
comptime val DB_URL: str = env("DATABASE_URL")  # build fails if unset

# Emitted Python
PORT: int = 8080
DB_URL: str = "postgresql://..."
```

### Readability features

#### Guards

`guard x = expr else: ...` is sugar for assignment plus an early-return on the falsy/None case. The checker narrows `x` to non-null after the guard.

#### Pipe operator

`a |> f() |> g(arg)` desugars to `g(f(a), arg)`. Left-associative; the pipe argument fills the first positional slot of the next call.

#### Extension methods

`extend str: def to_slug() -> str: ...` emits a free function `str_to_slug(self: str) -> str` and rewrites call sites `"hello".to_slug()` as `str_to_slug("hello")` at emit time. No monkey-patching of built-ins. The checker resolves method lookup against in-scope `extend` blocks.

## Type checker depth and approach

This is the most open-ended part of the project. Structural subtyping in particular needs care.

### Hybrid strategy

1. Run Typhon-specific checks on the Typhon AST: non-nullability, sealed-union exhaustiveness, `Result`/`?` propagation, `val`/`var`, no-implicit-`Any`, extension-method resolution.
2. Desugar to Python AST with rich type annotations preserved.
3. Optionally run `ty` (as a library, depending on the Ruff git repo) over the desugared AST to catch standard Python typing violations.

This split lets Typhon enforce its strict rules without re-implementing the entire Python typing spec, and lets `ty`'s mature engine handle the rest.

### Structural subtyping

The hardest piece. TypeScript's `tsc` checker is the reference: a polynomial-time decision algorithm with a memoised relation cache, recursive-type handling via assumed-subtype sets, and detailed diagnostics tracking which exact member is missing or incompatible. Port the algorithm in spirit; the implementation is its own months-long effort.

### Salsa queries

Express each analysis as a Salsa query: `parse(file)`, `resolve(module)`, `infer(function)`, `check(module)`. Salsa builds the dependency graph behind the scenes and recomputes only invalidated nodes when a file changes. Durability levels distinguish stdlib queries (rarely change) from user-file queries (change on every keystroke), saving hundreds of milliseconds per edit in rust-analyzer-scale projects.

## Code emission

The pipeline is: Typhon AST → desugar to plain Python AST (using the same `ruff_python_ast` node types as the parser produces) → `ruff_python_codegen` for source generation → `ruff_python_formatter` for Black-style reflow. The emitted file carries a generated-header comment (`# generated by ttc — do not edit`).

Source maps mapping `.py` line and column back to `.ty` are written as a sidecar `.py.map` file, similar to TypeScript. The LSP uses these for go-to-definition across the boundary, and a planned `ttc trace` command can map a Python traceback back to Typhon source.

There is deliberately no Typhon-specific runtime package the user must install. The handful of helpers needed (`Result`/`Ok`/`Err`, `lazy_import`, `str_to_slug`-style extension shims) are emitted inline into each project as a generated `typhon_runtime/` module the build owns. This keeps deployment exactly like deploying any other Python project.

## CLI and tooling

Single binary, `clap` subcommands:

| Command | Purpose |
|---------|---------|
| `ttc build` | Full pipeline: parse, check, analyse, desugar, emit, format. |
| `ttc check` | Up to analyser, no emit. Used by CI. |
| `ttc fmt` | Format `.ty` source. Wraps `ruff format` applied to a Typhon-aware pretty-printer. |
| `ttc lsp` | Run as a Language Server. |
| `ttc init` | Scaffold a new project: `typhon.toml`, `src/`, `tests/`. |
| `ttc trace` | Map a Python traceback back to Typhon source via `.py.map` files. |
| `ttc profile` | Instrument emitted code for hot-function detection (advanced, opt-in). |

### `typhon.toml`

```toml
[project]
name = "myapp"
version = "0.1.0"
src = "src"
out = "build"

[python]
target = "3.13"             # or "3.14"
free-threaded = false       # opt-in; requires 3.13t/3.14t

[emit]
class-default = "dataclass" # or "pydantic"
format = true               # post-process through ruff format

[strictness]
no-implicit-any = true
unused-import = "error"
exhaustive-match = "error"

[env]
required = ["DATABASE_URL"]  # comptime env() lookups must resolve at build time
```

## Roadmap

Realistic milestones for one person plus AI assistance. The headline target is a useful subset shippable in twelve months.

### Phase 0 — Foundation (months 1–2)

- Fork `ruff_python_parser` and `ruff_python_ast` into `vendor/`.
- Add one or two custom tokens (`val`, `var`) to confirm the fork-extend workflow.
- Round-trip Python through the fork via `ruff_python_codegen`: parse → emit, verify byte-identical (modulo whitespace) on a corpus of real Python files.
- `clap`-based `ttc` shell with `ttc fmt` working as the simplest end-to-end command.
- `miette` + `thiserror` diagnostic infrastructure.

### Phase 1 — Core types (months 3–5)

- Salsa db with `parse` and `resolve` queries.
- Name resolution and scope construction; `val`/`var` enforcement (no types yet).
- Nominal types: function signatures, assignment compatibility, primitive types, classes.
- Non-nullable by default with flow narrowing on guards and `isinstance` checks.
- `ttc check` produces useful "unknown name" and "type mismatch" diagnostics via `miette`.

### Phase 2 — Class and value features (months 6–8)

- Class emission as `@dataclass(slots=True)`; the `model` keyword for Pydantic.
- Sealed unions and exhaustive `match`. (This is high-value and mechanically simple — front-load it.)
- `Result[T, E]` type and the `?` operator; `with`-chains.
- Comptime constants with `env()` lookup. Build fails on missing required env.
- `tower-lsp-server` backend: diagnostics and hover working in VS Code.

### Phase 3 — Structural typing and advanced features (months 9–12)

- Generics (angle-bracket syntax, bidirectional inference, type erasure at emit).
- Interface declarations and structural subtyping with memoised relation cache.
- Pure-function detection (conservative syntactic check) and `@functools.cache` emission.
- Explicit `gather` block for `asyncio.gather`.
- Lazy imports and `lazy val`.
- Pipe operator, guards, extension methods.
- `.dty` stub files and `unsafe` blocks for untyped library interop.

At the end of Phase 3 — roughly month twelve — Typhon is useful for a real backend or CLI project. Everything beyond is polish and ambition.

### Phase 4+ — Beyond v1

- Automatic `asyncio.gather` inference (the conservative version that fires only on `@pure` straight-line code).
- Loop parallelisation for pure comprehensions on free-threaded Python.
- Richer comptime: `comptime` functions, types as values.
- PGO via `ttc profile`.
- LSP completions and code actions; go-to-definition across `.ty` and `.py` boundaries via source maps.
- Migration tooling from typed `.py` to `.ty` (`Optional[T]` → `T?`, dataclasses → Typhon classes, etc.).

## Risks and how to manage them

| Risk | Severity | Mitigation |
|------|----------|------------|
| Structural subtyping is months of work | High | Defer to Phase 3. Ship Phase 0–2 without interfaces; nominal types alone are enough to be useful. |
| Ruff parser API drifts under us | Medium | Pin to a SHA; review upstream weekly; keep diff against upstream small. |
| Salsa breaking changes between minor versions | Medium | Every Salsa call sits behind a one-function-wide wrapper module. |
| Free-threaded Python is moving target | Medium | Default off until 3.14 ships as default. Sequential fallback always available via `sys._is_gil_enabled()`. |
| Auto-parallelisation introduces races | High | Ship only the explicit `gather` keyword in v1. Inference comes later, conservative-only. |
| Pydantic coupling alienates users | Medium | Default emit is `@dataclass`, not `BaseModel`. Pydantic is opt-in via `model`. |
| Pre-emptive runtime helpers force a Typhon package on users | Medium | Emit `typhon_runtime/` as generated source the build owns; no PyPI package required. |
| Solo developer burnout on a multi-year project | High | Cut scope aggressively. The minimum-viable Typhon is non-null types + sealed unions + `Result` + dataclass emit. That alone is publishable. |

## Prior art worth studying

- **TypeScript**: closest analogue. Scanner → parser → binder → checker → emitter. The `checker.ts` file is the canonical reference for structural subtyping at scale.
- **rust-analyzer**: the cleanest example of a Salsa-based incremental compiler with an LSP. Crate layering directly transferable.
- **ty and Pyrefly**: Rust-based Python type checkers (Astral and Meta respectively). Both shipped in 2025; both architectural references for Typhon's checker.
- **oxc**: Rust-based JavaScript toolchain. Workspace layout (`oxc_parser`, `oxc_semantic`, `oxc_linter`, `oxc_formatter`, `oxlint` binary) is the template for `ttc`.
- **Mojo**: cautionary tale. Pitched as a "Python superset," then walked back. Lesson: be honest about what subset of Python `.ty` accepts and emit a clean error for the rest.
- **Cython, Coconut, Hy**: older Python supersets. Useful for emission patterns; none built on modern Rust tooling.

## Naming

The project ships as **Typhon**. The name keeps phonetic kinship with Python without sounding like a portmanteau, and the mythology lines up (Typhon is the serpent-monster of Hesiod, sometimes treated as the father of Python). The binary is `ttc`, the file extension is `.ty`, the stub extension is `.dty`, the config file is `typhon.toml`.

Quick check before committing: search PyPI for any active "typhon" packages (a couple of dormant ones exist) and confirm none clash with documentation or import names.

## What to build first

Concrete next steps, in order:

1. Set up the Cargo workspace skeleton with `crates/` and `vendor/` directories.
2. Get parse → emit round-tripping a real Python file (e.g. one of Django's management commands) without losing anything.
3. Add `val` and `var` as new keyword tokens. Confirm the fork-extend workflow is sustainable.
4. Wire up `clap` with `ttc fmt` as the first working command.
5. Add `miette` for diagnostics. Now any future error has somewhere good-looking to go.

That is roughly two months of work. Everything in this plan unfolds from those six steps.
