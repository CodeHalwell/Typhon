# Typhon — A Statically-Typed Superset of Python

## Harmonised Implementation Plan

## Executive summary

Typhon is a statically-typed, stricter superset of Python that compiles to clean, readable CPython 3.13+ code with no runtime dependency on the toolchain. Every `.ty` file emits valid, idiomatic `.py`; not all `.py` is valid Typhon. The compiler and language server live in a single Rust binary called `tyc`.

The architecture is a classical multi-stage transpiler: parser, type checker, analyser, desugarer, emitter, plus an embedded LSP. It piggy-backs on the modern Rust-Python ecosystem (Ruff for parsing and codegen, Salsa for incremental computation, ty and Pyrefly as architectural references, Pydantic v2 as an opt-in emit target, free-threaded Python 3.13t/3.14t for the parallelism story).

The risk in the project is not technological. Every individual stage has a mature, MIT-licensed Rust implementation to depend on, vendor, or learn from. The risk is scope. Two areas will dominate effort: structural type checking and inference, and keeping a forked grammar in sync with upstream Python syntax. A useful subset is shippable in roughly twelve months for one person with AI assistance; the full vision is a multi-year project.

## Goals and non-goals

### Goals

- **Static safety**: non-nullable by default, no implicit `Any`, explicit error handling via `Result[T, E]`.
- **Modern ergonomics**: `let`/`mut`, interfaces, sealed unions with exhaustive matching, guards, pipes, comptime, lazy loading.
- **Clean compilation** to standard Python with no runtime dependency on Typhon-specific machinery.
- **First-class tooling**: a single `tyc` binary that builds, checks, formats, and runs as an LSP, with sub-100 ms incremental feedback in the editor.
- **Honest interop** with existing Python via `.dty` stubs and explicit `unsafe` regions.

### Non-goals

- **Replacing CPython.** Typhon targets CPython 3.13+ (and the free-threaded build); it is not a new runtime.
- **Beating Pyrefly or ty on typing-spec conformance in v1.** Day-one Typhon supports only the subset of typing-spec features it needs.
- **Aggressive auto-parallelisation in v1.** The risky bits ship behind opt-in keywords first, inference later.
- **A novel package manager.** Standard pip/uv workflows are fine.

## Architecture

The `tyc` binary is a multi-stage compiler with an embedded LSP, structured as a Cargo workspace of small crates that mirror the pipeline stages. Each stage produces a typed Rust value that the next stage consumes; analysis results are stored as Salsa queries so the LSP can reuse them incrementally.

### Pipeline

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

### Workspace layout

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
│   └── tyc/                    thin CLI binary, clap subcommands
└── vendor/                     Typhon's in-tree fork of Ruff (pinned via vendor/UPSTREAM)
    ├── ruff_text_size/         TextSize / TextRange newtypes
    ├── ruff_source_file/       Line-index over a source string
    ├── ruff_python_trivia/     Whitespace + comment helpers
    ├── ruff_python_ast/        Python AST + Typhon's Mutability extension
    └── ruff_python_parser/     Lexer + parser, plus let/mut soft-keyword support
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
| Code emission | Hand-written pretty-printer (today) | Fork `ruff_python_codegen` (deferred) | Hand-written printer tracks line offsets for `.py.map`; upstream codegen lacks that hook. Post-process through `ruff format`. |
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

**Locked: PEP 695 bracket syntax** (`def f[T](x: T) -> T`, `type Vector[T: float] = ...`). The choice was forced by two factors: the vendored Ruff parser already accepts PEP 695, so the grammar work is zero; and divergence from CPython grammar is the dominant cost on a one-person project. The angle-bracket aesthetic kinship with TS/Rust loses on every load-bearing dimension.

Inference is bidirectional: constraints flow from arguments to type parameters (recursively, with conflict-widening into a union when bindings disagree) and substitute through the return type, falling back to explicit annotation when ambiguous. Bounded-type-var checking is wired up; full variance and higher-kinded forms remain partial. Generics are type-erased at emit time; runtime relies on Python's duck typing and (where present) Pydantic validation.

#### Interfaces (structural)

`interface` declarations are structural contracts, like Python's `typing.Protocol`. The checker verifies that a candidate type provides every required member with compatible signatures, with memoised "assumed subtype" sets to handle recursion. Interfaces emit as `typing.Protocol` subclasses.

PEP 544's `@runtime_checkable` only validates **attribute presence**, not signatures or types. A bare `isinstance(x, MyInterface)` therefore gives a much weaker guarantee than the static check. Typhon does not silently lower `is`-tests against an interface to `isinstance`; a runtime check against an interface either requires an explicit opt-in keyword or fails to compile. Static structural conformance remains the primary mechanism.

#### Sealed unions

`type Shape = Circle | Rectangle | Triangle` declares a finite, sealed sum type. `match` on a sealed union must cover every variant or include a wildcard; the checker compares the set of handled variants against the union's declared members and errors on missing cases. This is the single biggest static-safety win over current Python and is mechanically simple to implement.

#### No implicit `Any`

`Any` is a top type, but its inference is a compile error outside an explicit `unsafe` block. Untyped library calls must be wrapped in `unsafe` or shimmed with a `.dty` stub. This is strictly stricter than TypeScript's `noImplicitAny`.

**`unsafe` semantics.** `unsafe` is a **lexical region**, not a per-value annotation. Inside `unsafe { ... }` (or an `unsafe:` block), the checker:

1. Permits expressions whose inferred type would otherwise be `Any` to bind freely.
2. Tracks values originating from `unsafe` with a hidden `Unsafe[T]` marker that is invisible to user syntax but visible in diagnostics.
3. Refuses to let `Unsafe[T]` flow into a non-`unsafe` context where a concrete `T` is required — the value must be re-asserted via an explicit cast, narrowing, or assignment to an annotated `let`/`mut`.

This means "no `Any`" is enforced at every region boundary even when a region internally tolerates dynamism. Stub files (`.dty`) and explicit `unsafe` blocks are the only two ways dynamic typing enters a Typhon program.

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
from pydantic import BaseModel, ConfigDict

class ApiUser(BaseModel):
    model_config = ConfigDict(extra="forbid")

    id: int
    email: str
```

**`extra='forbid'` is the default.** Pydantic's stock default is `extra='ignore'`, which silently drops unexpected input. That contradicts Typhon's safety pitch, so `model` emission injects `extra='forbid'` by default. Users who want a permissive boundary opt in explicitly via `[emit] model-extra = "allow" | "ignore"` in `typhon.toml`, or per-class through a future modifier. `frozen=True` and similar configs remain opt-in; note that Pydantic immutability is *faux* — it blocks reassignment of fields but does not freeze nested mutable values. See `let` and `mut` below for what Typhon does and does not guarantee about immutability.

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

# Emitted Python (default: TaskGroup — cancels siblings on first failure)
async with asyncio.TaskGroup() as _tg:
    _t_user   = _tg.create_task(fetch_user(id))
    _t_posts  = _tg.create_task(fetch_posts(id))
    _t_notifs = _tg.create_task(fetch_notifications(id))
user   = _t_user.result()
posts  = _t_posts.result()
notifs = _t_notifs.result()
```

**Why `TaskGroup`, not `asyncio.gather`.** `asyncio.gather(...)` propagates the first exception to the caller but continues running the other awaitables in the background — a footgun when the siblings have side effects or hold resources. `asyncio.TaskGroup` (3.11+) cancels siblings on first failure, which matches Typhon's safety-first posture. The `gather` keyword therefore lowers to `TaskGroup` by default. A `gather(strategy="best-effort"):` form lowers to `asyncio.gather(..., return_exceptions=True)` for users who genuinely want partial-success semantics.

#### Free-threaded Python

Python 3.13 ships an experimental free-threaded build; 3.14 (Phase II) makes it officially supported with ~5-10% single-thread overhead. When `typhon.toml` sets `free-threaded = true`, the analyser is allowed to emit `ThreadPoolExecutor`-based parallelism for pure-function comprehensions on large collections. The emitter inserts a runtime `sys._is_gil_enabled()` check and falls back to sequential execution if the GIL is present. Default-off until 3.14 is the default Python.

#### `go` spawn

`go f(x)` schedules `f(x)` in the background: an `asyncio.Task` in async contexts, a `ThreadPoolExecutor.submit` future on free-threaded builds for CPU-bound functions. The form `go f(x) -> fut` binds the task handle.

**`go` does not lower to a bare `asyncio.create_task`.** Python's event loop holds only weak references to scheduled tasks, so a fire-and-forget `create_task(...)` whose handle is not retained can be garbage-collected mid-flight. `go` therefore lowers via a small helper in `typhon_runtime`:

```python
# typhon_runtime/tasks.py
_BACKGROUND: set[asyncio.Task] = set()

def spawn(coro):
    task = asyncio.create_task(coro)
    _BACKGROUND.add(task)
    task.add_done_callback(_BACKGROUND.discard)
    return task
```

`go f(x)` desugars to `typhon_runtime.tasks.spawn(f(x))`. The registry holds strong references for the task's lifetime and releases them on completion. The same pattern applies to thread-pool `go` on free-threaded builds, with a `Future` registry instead.

### `let` and `mut`

`let` and `mut` govern **binding immutability**, not deep value immutability. A `let u: User` cannot be reassigned, but if `User` exposes a mutable field, that field can still be written through. This matches Rust's `let` versus `let mut` and TypeScript's `const`; it does not match Clojure's deep immutability.

- `let` is immutable as a binding. Reassignment is a compile error.
- `mut` is mutable. Parallelisation passes refuse to touch any binding captured as `mut` by a spawned task without explicit synchronisation.
- Top-level module bindings default to `let` unless declared `mut`.

Deep immutability for class instances is an emit-time concern, not a binding one: pass `frozen=True` to the underlying `@dataclass` or Pydantic config (Pydantic's flavour blocks reassignment of fields but does not recursively freeze nested values). A future `freeze` modifier may layer stronger deep-immutability semantics on top, but `let` itself stays scoped to bindings.

### Lazy loading

`lazy import np = numpy` desugars to a `typhon_runtime.lazy_import_proxy` that defers module loading until first attribute access, built on `importlib.util.LazyLoader`. `lazy let` module-level bindings desugar to a cached getter. `lazy[list[T]]` return types emit generator functions instead of materialised lists.

### Stubs and Python interop

Typhon authors `.dty` stubs — Typhon-flavoured signatures for untyped third-party libraries. `.dty` is the source of truth, but it is **not** the interop format: every `.dty` is compiled to a standard PEP 561 `.pyi` stub during `tyc build` and written alongside the emitted `.py` (or into the package's stub directory when targeting a library build).

Why both:

- **`.dty`** keeps Typhon's stricter dialect: `T?`, `Result[T, E]`, sealed unions, interfaces, `unsafe` boundaries.
- **`.pyi`** is what mypy, pyright, Pyrefly, ty, and IDEs already understand. Emitting `.pyi` means Typhon users do not pay an interop tax to consume Typhon-authored libraries from plain Python code.

The emitter lowers Typhon-only forms back to typing-spec equivalents (`T?` → `T | None`, sealed unions → `Union[...]` with `Literal` tags where present, `Result[T, E]` → the runtime-helper classes referenced by their generated import path). Round-tripping is lossy in one direction by design: a `.pyi` consumed by Typhon does not automatically gain Typhon's stricter guarantees, and the checker treats incoming `.pyi` declarations as if they originated in an `unsafe` boundary unless an authored `.dty` overrides them.

Drift between `.dty` and the runtime module it describes is caught by an in-tree port of mypy's `stubtest`: `tyc check --stubs` compares the compiled `.pyi` against the runtime symbols of the implementation module and reports missing names, signature drift, and constructor arity mismatches.

### Compile-time evaluation (`comptime`)

A `comptime` binding or expression is evaluated by `tyc` at compile time inside a sandboxed interpreter that supports pure arithmetic, string operations, environment-variable lookup via `env(name, default?)`, simple container construction, and calls to other `comptime` functions. The result is inlined as a literal in the emitted Python. Anything outside the allowed set is a compile error.

This is the highest-leverage feature in the spec. Build-time env validation alone is worth shipping. Push it earlier in the roadmap than instinct suggests.

```python
# Typhon
comptime let PORT: int = int(env("PORT", "8080"))
comptime let DB_URL: str = env("DATABASE_URL")  # build fails if unset

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

### Purity inference and memoisation

A function is **inferable as pure** only if every one of the following holds:

1. Synchronous. Coroutines, generators, and async generators are excluded — their results depend on scheduling and consumption order.
2. All parameter types are hashable (primitives, frozen dataclasses, tuples of hashable types, sealed-union variants whose payloads are themselves hashable). Unhashable arguments would break dictionary-based caches.
3. No I/O calls in the transitive call graph: no `open`, no `socket`, no `subprocess`, no logger writes, no `print`, no DB drivers. The checker maintains a small allow-list and treats `unsafe`/stubbed calls as impure unless the stub itself is annotated `@pure`.
4. No reads from non-deterministic clocks or entropy sources (`time.time`, `time.monotonic`, `random.*`, `secrets.*`, `uuid.uuid4`, `os.urandom`).
5. No reads from or writes to mutable module-level state. Reads from `comptime let` bindings are fine; reads from a `mut` module binding are not.
6. No exceptions raised through the function in the inferred return paths — pure functions return `Result[T, E]` if they need to express failure.

When all six hold, the analyser may emit `@functools.cache` (unbounded) or `@functools.lru_cache(maxsize=N)` (bounded) at the user's preference, defaulting to `lru_cache(maxsize=1024)`. Caches are emitted only at the user's opt-in: a `@memo` attribute, a `[strictness] auto-memoise = true` toggle, or an explicit `@pure(memo=True)` annotation. The checker does not silently insert caches; if it could, it would also be silently extending the lifetime of every cached argument and return value.

Marking a function `@pure` manually that fails any of the six conditions is a hard error, not a warning.

## Type checker depth and approach

This is the most open-ended part of the project. Structural subtyping in particular needs care.

### Hybrid strategy

1. Run Typhon-specific checks on the Typhon AST: non-nullability, sealed-union exhaustiveness, `Result`/`?` propagation, `let`/`mut`, no-implicit-`Any`, extension-method resolution.
2. Desugar to Python AST with rich type annotations preserved.
3. Optionally run `ty` (as a library, depending on the Ruff git repo) over the desugared AST to catch standard Python typing violations.

This split lets Typhon enforce its strict rules without re-implementing the entire Python typing spec, and lets `ty`'s mature engine handle the rest.

### Structural subtyping

The hardest piece. TypeScript's `tsc` checker is the reference: a polynomial-time decision algorithm with a memoised relation cache, recursive-type handling via assumed-subtype sets, and detailed diagnostics tracking which exact member is missing or incompatible. Port the algorithm in spirit; the implementation is its own months-long effort.

### Salsa queries

Express each analysis as a Salsa query: `parse(file)`, `resolve(module)`, `infer(function)`, `check(module)`. Salsa builds the dependency graph behind the scenes and recomputes only invalidated nodes when a file changes. Durability levels distinguish stdlib queries (rarely change) from user-file queries (change on every keystroke), saving hundreds of milliseconds per edit in rust-analyzer-scale projects.

## Code emission

The pipeline is: Typhon AST → desugar to plain Python AST (using the same `ruff_python_ast` node types as the parser produces) → `ruff_python_codegen` for source generation → `ruff_python_formatter` for Black-style reflow. The emitted file carries a generated-header comment (`# generated by tyc — do not edit`).

Source maps mapping `.py` line and column back to `.ty` are written as a sidecar `.py.map` file, similar to TypeScript. The LSP uses these for go-to-definition across the boundary, and a planned `tyc trace` command can map a Python traceback back to Typhon source.

There is deliberately no Typhon-specific runtime package the user must install. The handful of helpers needed (`Result`/`Ok`/`Err`, `lazy_import`, `str_to_slug`-style extension shims) are emitted inline into each project as a generated `typhon_runtime/` module the build owns. This keeps deployment exactly like deploying any other Python project.

## CLI and tooling

Single binary, `clap` subcommands:

| Command | Purpose |
|---------|---------|
| `tyc build` | Full pipeline: parse, check, analyse, desugar, emit, format. |
| `tyc check` | Up to analyser, no emit. Used by CI. `--stubs` adds the `.dty` vs `.ty`/`.py` surface-API diff. |
| `tyc fmt` | Format `.ty` source. Wraps `ruff format` applied to a Typhon-aware pretty-printer. |
| `tyc lsp` | Run as a Language Server (diagnostics, hover, go-to-definition, completion, code actions). |
| `tyc init` | Scaffold a new project: `typhon.toml`, `src/`, `tests/`. |
| `tyc trace` | Map a Python traceback back to Typhon source via `.py.map` source maps. |
| `tyc profile` | Instrument emitted code for hot-function detection (advanced, opt-in). |
| `tyc migrate` | Convert typed Python (`.py`) into Typhon (`.ty`): `Optional[T]`/`T \| None` → `T?`, module-level annotated assigns gain `let`/`mut`, `@dataclass` decorators are dropped. |
| `tyc ty` | Build the project and run Astral's `ty` checker against the emitted Python (subprocess, opt-in). |
| `tyc repl` | Interactive Typhon evaluator — pipes each block through the full compile pipeline and a Python interpreter. |
| `tyc debug` | Build the project and launch the emitted Python under a debugger (default `pdb`). Repeatable `--break <ty-file>:<line>` flags translate Typhon source locations through `.py.map` and forward them into the debugger session. A full Typhon-native source-mapping debugger UI is still a follow-up. |
| `tyc run` | Execute a Typhon program. Defaults to the in-process `tyc-vm` tree-walking interpreter — no `.py` written, no CPython spawn. `--compile` falls back to build-then-exec for programs that import CPython libraries the VM doesn't speak natively. |
| `tyc explain <code>` | Print the catalog entry for a diagnostic code (mirrors `rustc --explain`). |
| `tyc cheatsheet` | Print the 30-second Typhon cheat sheet to stdout. |
| `tyc add` / `tyc remove` / `tyc sync` | Lightweight package-manager surface over `uv`: rewrite `[dependencies]` / `[dev-dependencies]` in `typhon.toml` and run `uv sync`. |

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
auto-memoise = false        # insert @functools.cache on inferred-pure functions
auto-gather = false         # fold straight-line independent `await` runs into TaskGroup
auto-parallel = false       # rewrite pure list comprehensions to a thread-pool map
parallel-min-size = 64
pgo-memoise = false         # promote hot pure fns to @functools.cache from typhon-profile.json
pgo-min-calls = 100

[env]
required = ["DATABASE_URL"]  # comptime env() lookups must resolve at build time

[dependencies]               # synced with `tyc add` / `tyc remove` / `tyc sync`
requests = ">=2.31"

[dev-dependencies]
pytest = "8.2"
```

## Roadmap

Realistic milestones for one person plus AI assistance. The headline target is a useful subset shippable in twelve months.

> **Status (May 2026):** Phases 0–3 are complete. Phase 5 — interop and
> developer experience — shipped in [v0.1.6](https://github.com/CodeHalwell/Typhon/releases/tag/v0.1.6):
> `plain class`, auto-skip for `Enum`/`Flag`/`ABC` parents plus a user-
> configurable `[emit] skip-decoration-bases` list, `class-default`
> validation, `or`/`and` truthy-union typing, generator→`Iterable`
> conformance, `tyc explain` / `tyc cheatsheet`, an upgraded `tyc init`
> scaffold, `.py`-in-`src/` copy-through, `tyc build --check`,
> `tyc::contains_secret_literal`, miette `url(...)` deep-links on every
> diagnostic with 50+ catalog pages, `tyc fmt` wrapping `ruff format`, and
> `tyc debug --break TY:LINE` source mapping. Phase 5.5 — constructor /
> method arity safety — shipped in
> [v0.2.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.2.0):
> `tyc::arg_count` now fires on auto-generated class constructors and
> `impl` methods; cross-module shape propagation arity-checks
> `from foo import Cls` and `import foo as f; f.Cls(…)` alike (both `.ty`
> source and `.dty` stubs); `tyc::missing_field_init` audits
> `X.__new__(X)` bypass construction patterns. Phase 6 — Python-annoyances
> surface — shipped in [v0.3.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.3.0):
> `newtype`, `freeze let`, `pub`, three new effect/safety diagnostics
> (`blocking_in_async`, `resource_not_managed`, `div_by_zero_literal`),
> and pre-built install artifacts for Linux + Windows alongside macOS.
> Correctness sweeps and additive features have layered in through
> [v0.3.1](https://github.com/CodeHalwell/Typhon/releases/tag/v0.3.1) (2026-05-22 stress round),
> [v0.4.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.4.0) (type-checker correctness),
> [v0.5.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.5.0) (post-v0.4 roadmap sweep —
> Salsa-cached LSP check path, `tyc debug` reads in Typhon coordinates,
> dict-comp parallelisation, `Type::TypeConstructor` HKT foundation,
> `comptime` types-as-values, three new `tyc migrate` rewrites, opt-in
> PyPI corpus sweep harness),
> [v0.5.1](https://github.com/CodeHalwell/Typhon/releases/tag/v0.5.1) (three emit-correctness fixes,
> deep VS Code grammar audit, 22 new stdlib-only examples),
> [v0.5.2](https://github.com/CodeHalwell/Typhon/releases/tag/v0.5.2) (`<ident>?` propagation in
> value position, `set - set` / `frozenset - frozenset` arithmetic,
> docs-site Typhon/Emitted-Python tabs sweep),
> [v0.6.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.6.0) (apps-feedback minor release
> — ten multi-file production-shaped reference apps under `examples/apps/`,
> `Ok` / `Err` Result-combinator methods, `impl` on a sealed-union alias,
> `tyc::stdlib_module_shadow`),
> [v0.6.1](https://github.com/CodeHalwell/Typhon/releases/tag/v0.6.1) (VS Code annotation-colour
> fix, `.py.map` sidecars under `<out>/.sourcemaps/`),
> [v0.7.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.7.0) (Round-3 apps-feedback
> carry-over: `pub *`, declare-only `let NAME: T`, `with cm() as r:`
> inference, `await Callable[..., Awaitable[T]]`,
> same-newtype-preserving arithmetic, cross-module generic method
> dispatch, ternary narrowing, `tyc::field_default_ordering`, five new
> apps under `examples/apps/`),
> [v0.7.1](https://github.com/CodeHalwell/Typhon/releases/tag/v0.7.1) (LSP
> semantic-tokens position alignment bugfix),
> [v0.8.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.8.0) (the
> stress-test sweep release), and
> [v0.8.1](https://github.com/CodeHalwell/Typhon/releases/tag/v0.8.1) — a
> point release fixing a v0.8.0 regression where the
> widened `tyc::attribute_not_found` rule false-positived on
> venv-introspected third-party Python classes
> (`uvicorn.Server.serve(...)`, `httpx.AsyncClient.aclose(...)`,
> `fastapi.Request.body(...)`); strictly a bugfix carve-out — and
> **[v0.9.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.9.0)** —
> the stress-test cleanup release closing **32
> findings** from a v0.8.1 stress sweep.
> **[v0.10.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.10.0)**
> is the VM completeness release (dunder dispatch + rich comparisons on
> user instances, finite generators, `type(x)` as a real type object,
> the long tail of missing builtins, pydantic `model_validate` /
> `model_dump`, and three type-checker exhaustiveness / augmented-assign
> fixes). The current release is
> **[v1.0.0-alpha.5](https://github.com/CodeHalwell/Typhon/releases/tag/v1.0.0-alpha.5)**
> (see `CHANGELOG.md` for the full v0.13.0 → v1.0.0-alpha.5 line). The
> milestone below,
> **[v0.12.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.12.0)**, brought
> VM comparison-protocol parity (`sorted` / `min` / `max` honour a user
> `__lt__`), the missing `dict` / `str` builtins, and **deep compile-time
> library introspection**: third-party argument-*type* checking across
> `tyc check` / `tyc build` and live in the editor via `tyc lsp`, plus Phase 1
> of the typeshed-backed `ty` integration. It builds on
> **[v0.11.0](https://github.com/CodeHalwell/Typhon/releases/tag/v0.11.0)**,
> the VM parity sweep that closes **22 findings** from a fresh
> v0.10.0 adversarial round and lands the `enum` keyword as a
> first-class declaration form (sugars over `enum.Enum` with
> `enum.auto()` for bare members). Two new VM value kinds —
> `Value::Complex` (native complex arithmetic, hashable for set / dict
> keys) and a dict-view kind backing `dict.keys()` / `.values()` /
> `.items()` — make complex numbers and dict views match CPython.
> Bare `super()` is rewritten to two-arg form so
> `@dataclass(slots=True)` no longer crashes; `__call__` /
> `__post_init__` dispatch; multi-level MRO field accumulation;
> native `enum` / `datetime` (naïve / UTC) / `pathlib` /
> `collections.defaultdict` shims; real `re.Match` capture groups;
> banker's `round`; `bytes` methods; `itertools.groupby(key=)`;
> `str.split(maxsplit=)`; f-string `{x=}`; `str %` runtime
> formatting. VM value semantics now match CPython: dataclass eq /
> repr / hash is value-based, set equality is order-independent,
> float repr matches CPython. Type checker tightens: `None` flows
> into `object`, `str %` is type-checked, builtin-scalar `.items()` /
> `5["a"]` / iteration fire at check time. The v0.9.0 VM is now usable as the
> daily-driver runner the docs always advertised: `Result`
> combinators, `open()` write/append/binary modes, class patterns on
> built-ins, `frozenset` as a dict key, deep `freeze let`, comptime
> inlining, `lazy import`, `class!` exception fields, dataclass
> mutable-default factories, `collections.deque` / `heapq` /
> `contextlib` / `pydantic` shims, multi-file projects, and
> `@property` / `super()` / `@contextmanager` all work under
> `tyc run`. The type checker plugs silent-correctness gaps in
> Sequence covariance, variant-to-parametric-union flow, `while
> True:` reachability, post-loop narrowing, `assert` narrowing,
> `*args` annotation policy, `extend list[T]` dispatch, exhaustive
> match on `T?`, `with`-chain error mismatch, and the `comptime let
> T: type` alias. The v0.8.0 feature surface
> closes 41 findings
> from a multi-file v0.7.1 stress report spanning the type checker, VM,
> parser, lowering passes, diagnostics, and CLI. Highlights:
> `tyc::attribute_not_found` now fires on class instances and generic
> classes (with foreign / venv-introspected classes tracked by a new
> `partial` shape marker that keeps the diagnostic lenient on
> third-party APIs); interface parameter type conformance; string-literal
> singleton types (`Type::LitStr`); arbitrary-precision integers in the
> VM (`num_bigint::BigInt`); insertion-ordered dicts
> (`indexmap::IndexMap`); full f-string format flags wired in the VM;
> mapping match patterns and sequence-with-star patterns; recursion
> limit raised to 1000; a much larger native VM stdlib (`re`, `typing`,
> `collections`, `functools`, `itertools`, `dataclasses`, `pathlib`);
> five parser scaffolds the docs already advertised
> (HKT `class Functor[F[_]]:`, `impl[T] SealedUnionAlias[T]:`,
> generic-plus-frozen `class X[T] frozen:`, `async def` in `interface`
> bodies, outer-annotation tuple unpack); `?` propagation in
> `with`-chains; `tyc::pattern_shadows_outer`; three new lint warnings
> (`empty_collection_no_annotation`, `typing_alias_in_annotation`,
> `contains_secret_literal`); synthetic-line sanitisation across every
> diagnostic; `tyc check lib.dty` single-file support; `tyc migrate`
> strips trivial `__init__` methods. Default change:
> `unused_import` is now `warn` (was `error`). Mostly additive on the
> accepted surface; the BigInt switch in the VM means programs that
> relied on silent i64 wrap-around now compute different (correct)
> results. Phase 4+ work is also landed: auto-gather, auto-parallel,
> PGO, LSP completions and code actions, cross-file go-to-definition,
> the package-manager surface, REPL, debugger, the `tyc-vm`
> tree-walking interpreter (default for `tyc run`), and `tyc migrate`.
> See [docs/roadmap.md](roadmap.md) for the canonical per-feature
> status.

### Phase 0 — Foundation (months 1–2) ✅ complete

- Fork `ruff_python_parser`, `ruff_python_ast`, `ruff_python_trivia`, `ruff_source_file`, and `ruff_text_size` into `vendor/`.
- Add `let` / `mut` soft keywords and a `Mutability` field on assignment AST nodes to confirm the fork-extend workflow.
- Round-trip Python through the fork: every emitted `.py` is verified by the integration test suite. A representative third-party corpus sweep is a future hardening task. (The original plan called for vendoring `ruff_python_codegen` as well; we instead ship a hand-written printer in `tyc-emit` because it tracks the per-statement line offsets needed for `.py.map` source maps. Vendoring `ruff_python_codegen` remains an open follow-up — see `tyc/vendor/README.md`.)
- `clap`-based `tyc` shell with `tyc fmt` working as the simplest end-to-end command.
- `miette` + `thiserror` diagnostic infrastructure.

### Phase 1 — Core types (months 3–5) ✅ complete

- Salsa db with `parse` and `resolve` queries.
- Name resolution and scope construction; `let`/`mut` enforcement (no types yet).
- Nominal types: function signatures, assignment compatibility, primitive types, classes.
- Non-nullable by default with flow narrowing on guards and `isinstance` checks.
- `tyc check` produces useful "unknown name" and "type mismatch" diagnostics via `miette`.

### Phase 2 — Class and value features (months 6–8) ✅ complete

- Class emission as `@dataclass(slots=True)`; the `model` keyword for Pydantic with `extra='forbid'` injected by default.
- Sealed unions and exhaustive `match`. (This is high-value and mechanically simple — front-load it.)
- `Result[T, E]` type and the `?` operator; `with`-chains.
- Comptime constants with `env()` lookup. Build fails on missing required env.
- `tower-lsp-server` backend: diagnostics and hover working in VS Code.

### Phase 3 — Structural typing and advanced features (months 9–12) ✅ complete

- **Generics syntax decision locked**: PEP 695 brackets (see *Open questions* below).
- Generics: bidirectional inference, type erasure at emit.
- Interface declarations and structural subtyping with memoised relation cache. `is`/`isinstance` against an interface is rejected unless explicitly opted into.
- `unsafe` block semantics: lexical regions with an `Unsafe[T]` boundary marker (see No-implicit-`Any`).
- Pure-function detection bound to the six-condition rule (sync, hashable args, no I/O, no entropy/clocks, no mutable module state, no exceptions). `@functools.cache` / `lru_cache` emission only with an explicit opt-in.
- `gather` block, lowered to `asyncio.TaskGroup` by default. `gather(strategy="best-effort"):` for the `asyncio.gather(..., return_exceptions=True)` shape.
- `go` lowered through `typhon_runtime.tasks.spawn` with a strong-ref registry.
- Lazy imports (`lazy import np = numpy` only — `lazy from x import a, b` is rejected because it defeats deferral) and `lazy let` (cached getter for module-level, `cached_property` for instance-level on effectively immutable objects).
- Pipe operator, guards, extension methods.
- `.dty` stub files, `.pyi` interop emission, and `tyc check --stubs` (stubtest port).

At the end of Phase 3 — roughly month twelve — Typhon is useful for a real backend or CLI project. Everything beyond is polish and ambition.

### Phase 4+ — Beyond v1 (in progress)

- ✅ Automatic `asyncio.gather` inference (opt-in via `[strictness] auto-gather`): straight-line independent `await` runs inside an `async def` fold into a `TaskGroup`.
- ✅ Loop parallelisation for pure list comprehensions on free-threaded Python (opt-in via `[strictness] auto-parallel`, threshold `parallel-min-size`).
- Richer comptime: `comptime` functions, types as values. (Not yet started.)
- ✅ PGO via `tyc profile` (opt-in via `[strictness] pgo-memoise`): promotes hot pure functions to `@functools.cache` from `typhon-profile.json`.
- ✅ LSP completions (visible bindings + Typhon keywords + common builtins) and a "Remove unused import" code-action quick-fix; cross-file go-to-definition across `.ty`/`.py` boundaries via the resolver's Salsa `resolved_module` query and the v2 `.py.map` source maps.
- ✅ Migration tooling from typed `.py` to `.ty` (`Optional[T]` → `T?`, `let`/`mut` on annotated assigns, `@dataclass` decorators dropped) via `tyc migrate`.
- ✅ Interactive REPL (`tyc repl`) and a pdb-launcher debugger (`tyc debug`) with `--break TY:LINE` source-mapped breakpoints.
- ✅ Package-manager surface over `uv` (`tyc add` / `tyc remove` / `tyc sync`).
- ✅ `tyc run` defaults to the in-process `tyc-vm` tree-walking interpreter — no `build/`, no CPython spawn. `--compile` (alias `--no-vm`) falls back to the legacy build-then-exec path for programs that import CPython libraries the VM doesn't speak natively. See [docs/vm.md](vm.md).
- ✅ Phase 5 interop + DX bundle (v0.1.6): `plain class`, `[emit] skip-decoration-bases`, `class-default` validation, `or`/`and` truthy-union typing, generator→`Iterable` conformance, `tyc explain` / `tyc cheatsheet`, `tyc build --check`, `.py`-in-`src/` copy-through with `tyc::orphan_py_import`, `tyc::contains_secret_literal`, miette `url(...)` deep-links on every diagnostic, `tyc fmt` wrapping `ruff format`. See [docs/roadmap.md](roadmap.md#phase-5--interop-and-developer-experience--complete-v016).
- ✅ Phase 5.5 constructor / method arity safety (v0.2.0): `tyc::arg_count` now fires on auto-generated `__init__` of `class` / `model` declarations and on `impl` methods; cross-module shape propagation arity-checks `from foo import Cls` and `import foo as f; f.Cls(…)` alike; `.ty` source and `.dty` stubs participate on equal footing through a project-wide `ExternalShapes` registry; the LSP caches per-file extraction via a Salsa-tracked `module_shapes_query`; a new `tyc::missing_field_init` post-construction audit catches `X.__new__(X)` bypass patterns where the instance escapes without required fields assigned. See [docs/roadmap.md](roadmap.md#phase-55--constructor--method-arity-safety--complete-v020).
- ✅ Phase 6 Python-annoyances surface (v0.3.0 — correctness sweeps in v0.3.1 / v0.4.0; additive features and polish through v0.5.0 / v0.5.1 / v0.5.2 / v0.6.0 / v0.6.1 / v0.7.0 / v0.7.1 / v0.8.0 / v0.8.1 / v0.9.0 / v0.9.1 / v0.9.2 / v0.10.0 / v0.11.0 / **v0.12.0** (current release)). v0.3.0 introduced `newtype` (nominal aliases over primitives), `freeze let` (deep-frozen module-level bindings), `pub` (`__all__`-synthesising visibility marker), three new effect / safety diagnostics (`blocking_in_async`, `resource_not_managed`, `div_by_zero_literal`), and pre-built install artifacts for Linux + Windows. v0.5.0 added the `Type::TypeConstructor` HKT foundation, `comptime` types-as-values, dict-comp parallelisation, and three new `tyc migrate` rewrites (frozen-`@dataclass` → `class X frozen:`, `Protocol` → `interface`, `NewType` → `newtype`). v0.6.0 shipped ten production-shaped reference apps under `examples/apps/` and three additive features (`Ok` / `Err` Result-combinator methods, `impl` on a sealed-union alias distributing to every variant, `tyc::stdlib_module_shadow`). v0.7.0 (the Round-3 apps-feedback carry-over) added `pub *` wildcard re-export aggregation in `__init__.ty`, declare-only `let NAME: T` with arm-assignment, `with cm() as r:` inference, `await Callable[..., Awaitable[T]]` unwrapping, same-newtype-preserving arithmetic, and cross-module generic method dispatch. v0.7.1 is a strict LSP bugfix point release. v0.8.0 (the stress-test sweep release on top of v0.7.1) closes 41 findings from a multi-file stress report. Type-system widening: `tyc::attribute_not_found` fires on class instances and generic classes (with foreign / venv-introspected classes tracked by a new `partial` shape marker), interface parameter type conformance, string-literal singleton types (`Type::LitStr`), `?` propagation in `with`-chains, `tyc::pattern_shadows_outer`, `newtype`-invalid-base rejection. VM upgrades: `num_bigint::BigInt` arbitrary-precision integers, `indexmap::IndexMap` insertion-ordered dicts, full f-string format flags, mapping match patterns, sequence-with-star patterns, recursion limit raised to 1000, larger native stdlib (`re`, `typing`, `collections`, `functools`, `itertools`, `dataclasses`, `pathlib`). Parser scaffolds: HKT `class Functor[F[_]]:`, `impl[T] SealedUnionAlias[T]:`, generic-plus-frozen `class X[T] frozen:`, `async def` in `interface` bodies, outer-annotation tuple unpack. Diagnostics: synthetic-line sanitisation across every diagnostic, three new lint warnings (`empty_collection_no_annotation`, `typing_alias_in_annotation`, `contains_secret_literal`), better hints on `wrong_arg_count` / collection variance / dict-to-model mismatch. CLI: `tyc check lib.dty` single-file support, `tyc migrate` strips trivial `__init__`. Default change: `unused_import` is now `warn` (was `error`). **v0.9.0 — the stress-test cleanup release — closes 32 of the 36 findings from a v0.8.1 stress sweep. VM coverage:** `Result` combinators (`.map` / `.map_err` / `.and_then` / `.or_else`) work on `Ok` / `Err`; `open()` honours write/append/binary modes plus `__enter__`/`__exit__`; class patterns on built-in types (`case str() as s:`); `frozenset(...)` is hashable as a dict key (`HashKey::FrozenSet`); native shims for `collections.deque` / `heapq` / `contextlib.contextmanager` / `pydantic.BaseModel`; `@property` / `@classmethod` / `@staticmethod` / `super()` builtins; `lazy import np = numpy` uses the simpler rewrite; multi-file projects load sibling `.ty` modules via the project source root and `tyc run --compile` spawns `python -m <pkg>.main`; `dataclasses.field(default_factory=list)` invokes per-instance; `class!` synthesised `__init__` runs and exception-type matching walks the MRO; `freeze let` actually freezes recursively; `comptime let X = ...` inlines via the substitution pass; typed tuple unpack parses. **Type checker:** read-view covariance for built-in containers (`list[Dog] → Sequence[Animal]`); variant → parametric sealed union assignability (`Cons[T] → LinkedList[T]`); `while True:` reachability + post-loop narrowing; `assert x is not None` narrows; `*args` / `**kwargs` require annotations (canonical `object`); `extend list:` dispatches on `list[T]` receivers; exhaustive `match` on `T?` recognises class patterns; `with`-chain `else err: return Err(err)` validates the error type; `func[T](args)` instantiation fires a clear check-time error; `comptime let T: type = int` lowers to a PEP 695 alias; new `tyc::freeze_not_freezable` validates `freeze let` RHS at check time; `pub *` collisions surface in `tyc check` (not just build). **Diagnostics polish:** `interface_not_conforming` arity message rephrased; `invalid_question_op` covers comprehension carve-out; sealed-union impl distribution dedupes diagnostics by `(code, message)`; `class_attr_shadows_slot` no longer false-positives on mutable-default fields; `MissingAnnotation` drops double-backtick wrapping. **Docs:** cheat sheet documents `class X frozen(Base):` ordering and the `*args: object` idiom. **v0.9.1** fixes four `tyc fmt` round-trip corruption modes (`impl Alias:`, `frozen`, `pub *`, multi-line kwarg respacing) and a `pub *` facade hole in sealed-union exhaustiveness. **v0.9.2** seeds `class_parents` from `InterfaceShape.bases` during cross-module shape ingestion so a `class! Sub(Foreign):` imported across modules no longer false-positives `tyc::attribute_not_found`. **v0.10.0 — the VM completeness release — closes the dunder-dispatch and builtins-coverage gaps that stopped `tyc run` from being a drop-in for `tyc build && python`. VM:** dunder dispatch + rich comparisons on user instances (`__add__` + reflected forms, `__eq__` / `__lt__` / …, `__str__` / `__repr__` / `__len__` / `__getitem__` / `__contains__`, `@property` getters, `@classmethod` `cls`-binding, inherited through bases); finite generators (`yield` / `yield from`) via eager materialisation capped at 1M items; `type(x)` returns a real type object (`type(x).__name__`, `type(x) == int`); the long tail of missing builtins (`divmod`, `pow` 2-/3-arg, `format`, `ascii`, `int(str, base)` incl. base=0, full set algebra, the missing string methods, `dict(other)` / `dict(**kwargs)`, `math.gcd` / `lcm` / `factorial` / `isqrt` / `comb` / `perm`, `json.dumps(indent=…)`, `time.perf_counter` / `process_time`); `max` / `min` / `list.sort` accept `key=` / `reverse=` / `default=` kwargs; pydantic `model_validate` / `model_dump` / `model_dump_json` for flat models; `enumerate` / `zip` / `map` / `filter` iterator-adapter panic fixed. **Type checker:** exhaustive `match` over `bool` / string-literal unions / fixed-arity tuples no longer false-positives `missing_return`; augmented assignment is type-checked on scalar targets (`s += 5` fires `operator_type_mismatch`); `allow_secret_comptime` strictness knob wired through. **Emit:** integer / float / char / string literal emission switched to stack-allocated `itoa` / `ryu` buffers, eliminating per-call heap allocations. **v0.11.0 — the VM parity sweep — closes 22 findings from a fresh v0.10.0 adversarial stress round and lands the `enum` keyword as a first-class declaration form. Language:** `enum Name:` sugars over `enum.Enum`, bare members auto-fill with `enum.auto()`, explicit `MEMBER = value` is preserved, `tyc fmt` round-trips. **VM value kinds:** `Value::Complex` (native complex arithmetic, promotes across int / float, hashable for set / dict keys, reflected dunders dispatch) and a dict-view kind backing `dict.keys()` / `.values()` / `.items()` (repr / iterate / `in` / `len` / re-iterable match CPython). **VM runtime:** native `enum` module (`enum.auto`, declaration-order iteration, `<Shape.CIRCLE: 1>` repr); bare `super()` rewritten to two-arg `super(EnclosingClass, self)` in `tyc-desugar` so `@dataclass(slots=True)` no longer crashes; `__call__` dispatch on callable instances; `__post_init__` invoked after auto-generated construction; multi-level inheritance field accumulation across the full MRO; instance operator-dunder dispatch reaches every numeric / bitwise / matmul slot with reflected fallback; subscript `__missing__` hook backs `defaultdict`. **VM stdlib:** native `collections.defaultdict(factory)`, native `datetime` (naïve / UTC), native `pathlib` (`/` join via `__truediv__`, `.parent` / `.name` / `.stem` / `.suffix` / `.suffixes` / `.parts`), real `re.Match.group` / `.groups` / `.groupdict` capture groups, banker's `round`, `bytes` methods (`decode` / `hex` / `fromhex` / `count` / `find` / `rfind` / `startswith` / `endswith` / `split` / `strip`), `itertools.groupby(key=)`, `str.split(maxsplit=…)` as pure-keyword arg, f-string `{x=}` debug conversion, `str %` / f-string `%` runtime formatting. **VM value-semantics fixes (align with CPython, were silent-wrong under v0.10.0):** dataclass instance equality is value-based with class-identity keying (no more cross-module same-name collisions); dataclass `repr` is `Name(field=value, …)`; dataclass instances hashable via `HashKey::Instance`; set / frozenset equality is order-independent; set / frozenset repr sorts elements by canonical key; float `repr` matches CPython's shortest round-tripping form with scientific notation for exp < -4 or ≥ 16. **Type checker:** `None` flows into `object`, `str %` is type-checked, builtin-scalar `(5).items()` / `5["a"]` / `for x in 5:` fire at check time. **Tooling:** `tyc init` seeds `allow-secret-comptime = false` in the generated `typhon.toml`; `tyc-introspect` retries transient fork failures with exponential backoff. **Emit:** decorator-list matching and complex-number emission share stack buffers, no per-call heap allocation.
- ✅ Runtime `stubtest` probe via `tyc stubtest` (shells out to `python -m mypy.stubtest`) — complements the AST-level `tyc check --stubs` diff.
- `ty` integration as a complementary second-stage checker over the desugared Python — see [docs/ty-integration.md](ty-integration.md). (Subprocess form landed as `tyc ty`; the embedded-library form is deferred.)

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
| Generics syntax choice locks parser fork shape | Resolved | Locked to PEP 695 brackets at Phase 3 entry — see *Open questions*. The vendored Ruff parser accepts them natively and the emitter round-trips them unchanged. |
| `go` tasks GC'd mid-flight (weak refs in event loop) | Medium | Lower `go` through `typhon_runtime.tasks.spawn`, never to a bare `asyncio.create_task`. Strong-ref registry with done-callback cleanup. |
| `asyncio.gather` exception semantics surprise users | Medium | Default `gather` block lowers to `TaskGroup` (cancels siblings on first failure). Reserve `gather(strategy="best-effort")` for the legacy `gather(...)` behaviour. |
| Pydantic's default `extra='ignore'` silently drops input | Medium | `model` emission injects `extra='forbid'` by default. Permissive modes are opt-in via `typhon.toml`. |
| `.pyi` interop drift from `.dty` source | Medium | `tyc check --stubs` runs an in-tree `stubtest` port against runtime modules; CI gates on it. |
| Auto-memoisation extends object lifetimes invisibly | Medium | Never silently insert `@functools.cache`. Require `@memo`, `@pure(memo=True)`, or an explicit project-wide opt-in; six purity conditions must hold. |

## Prior art worth studying

- **TypeScript**: closest analogue. Scanner → parser → binder → checker → emitter. The `checker.ts` file is the canonical reference for structural subtyping at scale.
- **rust-analyzer**: the cleanest example of a Salsa-based incremental compiler with an LSP. Crate layering directly transferable.
- **ty and Pyrefly**: Rust-based Python type checkers (Astral and Meta respectively). Both shipped in 2025; both architectural references for Typhon's checker.
- **oxc**: Rust-based JavaScript toolchain. Workspace layout (`oxc_parser`, `oxc_semantic`, `oxc_linter`, `oxc_formatter`, `oxlint` binary) is the template for `tyc`.
- **Mojo**: cautionary tale. Pitched as a "Python superset," then walked back. Lesson: be honest about what subset of Python `.ty` accepts and emit a clean error for the rest.
- **Cython, Coconut, Hy**: older Python supersets. Useful for emission patterns; none built on modern Rust tooling.

## Open questions

These are decisions deferred but consequential enough to record. Each must be resolved before the relevant phase begins.

### Generics syntax — resolved: PEP 695 brackets

Locked at Phase 3 entry. PEP 695 (`def f[T](x: T)`, `type Vector[T: float] = ...`)
wins on every load-bearing dimension for a one-person project:

| | Angle brackets `<T>` | PEP 695 brackets `[T]` (chosen) |
|---|---|---|
| Parser fork cost | Higher: ambiguous with comparison operators, needs lookahead | Zero: the vendored Ruff parser already accepts it |
| Lowering cost | Heavier rewrite into `[T]` / `TypeVar` shapes | Pass-through — same syntax Python emits |
| Long-term divergence from CPython | Grows with every Python release | Stays in lockstep |

We retain the angle-bracket aesthetic only as a possible future surface-only
sugar, not as the canonical form.

### `unsafe` granularity — block, expression, or both

The spec defines `unsafe` as a lexical block. A per-expression `unsafe expr` form would let users sprinkle dynamism more narrowly, at the cost of a noisier surface. Stick with blocks for v1; revisit if real codebases show a pattern of `unsafe { single_call() }`.

### `Result[T, E]` versus exceptions at the Typhon/Python boundary

Inside Typhon, `Result` is the preferred error channel. At the boundary with Python, two policies are possible: (a) Python exceptions raised by called code propagate through Typhon as exceptions, and the user catches with `try/except`; (b) the boundary catches all exceptions and lifts them into `Result[T, Exception]`. Option (a) wins for stack-trace clarity and Python-ecosystem fidelity; option (b) wins for purity of the error model. Default to (a), keep (b) as an explicit `unsafe.lift` form.

### Pydantic default — boundary types only, or every model

`model` is opt-in via keyword. The question is whether Typhon's standard library and stub set should *prefer* `model` for boundary types (HTTP, DB rows, CLI args) by convention, or stay agnostic. Recommendation: ship example projects and stubs that use `model` only where validation matters; do not encode this as a checker rule.

### Async public APIs when internals infer async

If Phase 4+ ever introduces async inference, public API stability requires that an inferred-async function does not silently change colour on Python consumers. Likely answer: inference applies to file-internal functions only; exported functions are always explicit. Record formally when async inference work begins.

## Naming

The project ships as **Typhon**. The name keeps phonetic kinship with Python without sounding like a portmanteau, and the mythology lines up (Typhon is the serpent-monster of Hesiod, sometimes treated as the father of Python). The binary is `tyc`, the file extension is `.ty`, the stub extension is `.dty`, the config file is `typhon.toml`.

Quick check before committing: search PyPI for any active "typhon" packages (a couple of dormant ones exist) and confirm none clash with documentation or import names.

## What to build first

Concrete next steps, in order:

1. Set up the Cargo workspace skeleton with `crates/` and `vendor/` directories.
2. Get parse → emit round-tripping a real Python file (e.g. one of Django's management commands) without losing anything.
3. Add `let` and `mut` as new keyword tokens. Confirm the fork-extend workflow is sustainable.
4. Wire up `clap` with `tyc fmt` as the first working command.
5. Add `miette` for diagnostics. Now any future error has somewhere good-looking to go.

That is roughly two months of work. Everything in this plan unfolds from those six steps.
