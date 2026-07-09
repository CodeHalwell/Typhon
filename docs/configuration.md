# `typhon.toml` Reference

> Excerpted from the [long-term plan](long-term-plan.md). The plan is the source of truth.

Each Typhon project has a `typhon.toml` at its root, written by `tyc init` and read by every subcommand.

Dependencies can be managed externally with `uv`/`pip`, or declared inline in `[dependencies]` / `[dev-dependencies]` and synced with `tyc add` / `tyc remove` / `tyc sync` — those commands rewrite the manifest and shell out to `uv` for the install step.

## Full example

```toml
[project]
name = "myapp"
version = "0.1.0"
src = "src"
out = "build"

[python]
target = "3.13"             # or "3.14" / "3.15"
free-threaded = false       # opt-in; requires 3.13t/3.14t/3.15t

[optimise]
level = 0                   # 0 (default) | 1 — level 1 flips auto-memoise/auto-gather/auto-parallel/pgo-memoise on by default

[emit]
class-default = "dataclass" # only "dataclass" today (use the `model` keyword per class for pydantic)
format = true               # post-process through ruff format
skip-decoration-bases = []  # extra base classes that suppress @dataclass injection
model-extra = "forbid"      # ConfigDict(extra=…) for model classes: "forbid" | "ignore" | "allow"
traceback-remap = false     # auto-install a .ty-source traceback remapper in the entry __main__

[strictness]
no-implicit-any = true
unused-import = "error"
exhaustive-match = "error"
methods-in-class-body = "warn"  # severity for Rule-4 violations (methods belong in `impl Name:`)
auto-memoise = false        # opt-in: insert @functools.cache on inferred-pure functions (defaults on at [optimise] level = 1)
auto-gather = false         # opt-in: fold straight-line independent `await` runs into TaskGroup (defaults on at level 1)
auto-parallel = false       # opt-in: rewrite pure list comprehensions to a thread-pool map (defaults on at level 1)
auto-parallel-reductions = false  # opt-in: parallelise integer accumulator loops (requires auto-parallel)
parallel-min-size = 64      # minimum iterable size for auto-parallel to fire
parallel-backend = "threads"  # "threads" (default) | "interpreters" — backend for auto-parallel rewrites
pgo-memoise = false         # opt-in: promote hot pure fns to @functools.cache from typhon-profile.json (defaults on at level 1)
pgo-min-calls = 100         # minimum profile-recorded call count for pgo-memoise to fire
suggest-perf = true         # advice: surface the tyc::perf_* micro-optimisation lint family
suggest-parallel = true     # advice: surface tyc::parallel_opportunity on parallelisable loops
stub-check = "error"        # severity for tyc::stub_mismatch from `tyc check --stubs`
unintrospectable-dependency = "warn"  # "warn" | "error" | "off" — declared dep imported but not introspectable
allow-secret-comptime = false  # set true to silence tyc::contains_secret_literal

[checker]
external = "none"           # or "ty" — second-stage check over emitted Python (typeshed-backed)
external-args = []          # extra flags forwarded verbatim to the external checker

[env]
required = ["DATABASE_URL"]  # comptime env() lookups must resolve at build time

# Inline dependency management — synced with `tyc add` / `tyc remove` / `tyc sync`,
# which write a generated `pyproject.toml` and shell out to `uv`.
[dependencies]
requests = ">=2.31"
rich = "*"                  # bare name → any version

[dev-dependencies]
pytest = "8.2"              # bare version → ==8.2
```

## Sections

### `[project]`

| Key | Type | Description |
|-----|------|-------------|
| `name` | string | Project name. |
| `version` | string | Semver. |
| `src` | path | Source directory (default `src/`). |
| `out` | path | Emit directory (default `build/`). |

### `[python]`

| Key | Type | Description |
|-----|------|-------------|
| `target` | `"3.13"` \| `"3.13t"` \| `"3.14"` \| `"3.14t"` \| `"3.15"` \| `"3.15t"` | Target CPython version. **Typhon requires 3.13+**; older targets are rejected at config load with `unsupported [python] target` and an actionable error. The `t` suffix selects free-threaded emission paths (PEP 703); see `free-threaded` below. A `3.15`+ target additionally unlocks native [PEP 810](https://peps.python.org/pep-0810/) lazy-import lowering — `lazy import` emits the native `lazy import MODULE as ALIAS` statement instead of the `typhon_runtime` helper call (see the lazy-loading section of `language.md`). |
| `free-threaded` | bool | Allow free-threaded emission paths. Default `false` until 3.14 is the default Python. Use a `t` suffix on `target` to attest at config time. |

### `[emit]`

| Key | Type | Description |
|-----|------|-------------|
| `class-default` | `"dataclass"` | Default emit target for `class` declarations. Only `"dataclass"` is implemented today; a project-wide `"pydantic"` default is **not yet wired** and is rejected at config load (declare boundary types with the per-class `model` keyword instead). Unknown values (`"struct"`, `"plain"`, `"none"`, the empty string, …) are likewise rejected with `tyc::invalid_config_value` rather than silently treated as `"dataclass"`. |
| `format` | bool | Post-process emitted `.py` through `ruff format`. |
| `skip-decoration-bases` | list of strings | Extra base-class names that suppress the automatic `@dataclasses.dataclass(slots=True)` decorator and trigger plain-class emission with a synthesised `__init__` calling `super().__init__()`. Matched by *last segment*, so `["BaseModel", "MyCustomBase"]` catches `pydantic.BaseModel` and `mylib.frameworks.MyCustomBase` regardless of how they're imported. Built-in entries (`Protocol`, `Enum`, `IntEnum`, `Flag`, `IntFlag`, `StrEnum`, `ABC`, `NamedTuple`, `BaseModel`, `App`) are auto-skipped without needing to be listed. |
| `model-extra` | `"forbid"` \| `"ignore"` \| `"allow"` | Value for `ConfigDict(extra=…)` injected into every `model` class. Default `"forbid"` rejects unexpected fields at runtime. `"ignore"` silently drops them; `"allow"` passes them through as extra attributes. Unknown values are rejected at config load. |
| `traceback-remap` | bool (default `false`) | When `true`, inject `typhon_runtime.traceback.install()` into the entry module's `if __name__ == "__main__":` block so an uncaught exception's traceback is rewritten to point at `.ty` source automatically (the same mapping `tyc trace` applies), with no manual step. Only the entry script is affected — library imports never trip the `__main__` guard — and the hook falls back to the previous behaviour on any failure, so it can only improve a traceback. Default-off keeps existing projects and runtime-free entry points dependency-free. |

> **Always-on behaviour.** Two emit policies that used to be exposed in
> `typhon.toml` are no longer configurable and run unconditionally:
>
> - **PEP 561 `.pyi` stubs** are emitted for every `.dty` file next to the
>   project, so mypy / pyright / Pyrefly / `ty` can consume Typhon-authored
>   libraries without an interop tax. The previously-documented
>   `[emit] pyi-stubs` toggle has been removed.
> - **Pydantic `model` classes** inject `model_config = ConfigDict(extra=…)`
>   where the `extra` value is controlled by `[emit] model-extra`
>   (default `"forbid"`). Accepted values: `"forbid"`, `"ignore"`, `"allow"`.

### `[optimise]`

A single project-wide optimisation dial.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `level` | int (`0` \| `1`) | `0` | `0` leaves every optimisation knob at its own default. `1` flips the **default** of the four optimisation strictness knobs — `auto-memoise`, `auto-gather`, `auto-parallel`, `pgo-memoise` — to `true`. Any other integer is rejected at config load; a non-integer value is a parse error. |

**Explicit-wins rule.** `level = 1` only changes the *default* of the four knobs. An explicit `[strictness]` entry for any of them always overrides the level-derived default, so you can opt into level 1 while keeping one knob off:

```toml
[optimise]
level = 1                   # auto-memoise / auto-gather / pgo-memoise default on…

[strictness]
auto-parallel = false       # …but this explicit entry keeps auto-parallel off
```

`tyc build -O` / `--optimise` (alias `--optimize`) applies `level = 1` for a single invocation without editing `typhon.toml` — it overrides a config `level = 0` but, like the toml `level`, never overrides an explicit `[strictness]` setting. See [cli.md](cli.md#tyc-build).

### `[strictness]`

| Key | Type | Description |
|-----|------|-------------|
| `no-implicit-any` | bool | Reserved — surfaces today as the `tyc::missing_annotation` diagnostic on un-annotated parameters and return types. The flag is parsed for forward compatibility; toggling it does not currently relax the check. |
| `unused-import` | `"error"` \| `"warn"` \| `"off"` | Severity for unused imports. |
| `exhaustive-match` | `"error"` \| `"warn"` \| `"off"` | Severity for non-exhaustive `match` over sealed unions. |
| `methods-in-class-body` | `"error"` \| `"warn"` \| `"off"` | Severity for `tyc::method_in_class_body` (Rule 4: methods live in `impl Name:`, not the class body). Default `"warn"` matches every other nudge diagnostic. Promote to `"error"` to break CI once your codebase has migrated. `"off"` suppresses the diagnostic entirely — useful for codebases still mid-migration. |
| `auto-memoise` | bool | Whether to apply `@functools.cache` to functions the analyser infers as pure. Default `false`, but **defaults to `true` at `[optimise] level = 1`** (an explicit entry here always wins). Caches are *never* inserted silently: even when enabled, the analyser requires all six purity conditions (see [language.md](language.md)). |
| `auto-gather` | bool | When `true`, runs of two-or-more consecutive independent `name = await callee(...)` statements inside an `async def` are folded into an `asyncio.TaskGroup` so they execute concurrently. Independence is decided by static data-flow on bound names; **every callee must be a same-module `async def` carrying an explicit `@gatherable` decorator** — undecorated async functions and externally-imported callees are left alone, so opting a project into `auto-gather` does not surprise callers that aren't ready to run concurrently. Default `false` (defaults to `true` at `[optimise] level = 1`; explicit entry wins). Explicit `gather:` blocks are unaffected. |
| `auto-parallel` | bool | When `true`, list / set / dict comprehensions whose element is a pure call are rewritten at build time into a `typhon_runtime.parallel.map_pure` thread-pool map. Handles the baseline `[f(x) for x in xs]` plus three widened shapes: a pure `if` filter (runs sequentially in the map source), extra call arguments that are literals or `let`-bound loop invariants (a `mut`-bound name is never captured), and nested pure calls (`g(f(x))`). Combine with `[python] free-threaded` to release the GIL across workers; on stock CPython the rewrite still runs but the GIL serialises workers. Default `false` (defaults to `true` at `[optimise] level = 1`; explicit entry wins). |
| `auto-parallel-reductions` | bool | When `true`, integer accumulator loops (`for x in xs: total += f(x)`) over a pure body are also eligible for the parallel-reduction rewrite — `total += sum(map_pure(lambda x: f(x), xs))`. **Requires `auto-parallel` to have any effect.** **Integers only**: the accumulator must be declared `mut total: int`. Integer addition is associative/commutative and Python ints are exact, so summing partial results in any order is identical; a `float` accumulator is never rewritten because reordering IEEE-754 addition changes the result. The iterable must additionally be **provably bounded and effect-free to materialise** (`map_pure` runs `list(ITER)` before evaluating any element): a `list`/`tuple`/`set` literal, a bare name annotated `list[...]` / `tuple[...]` / `set[...]` / `frozenset[...]` in the loop's scope, or a direct builtin `range(...)` call — note that parallelising a `range` loop materialises the range, an inherent cost of the map-based design. Call results, unannotated names, and generators never rewrite (an unbounded iterator would hang where the sequential loop raises on its first element; a stateful one would run side effects the loop never reached). Default `false`. |
| `parallel-min-size` | int | Minimum statically-detectable iterable length for a comprehension or reduction loop to qualify for `auto-parallel`. Default `64`. When the iterable size cannot be inferred, the threshold is treated as zero — users opting in accept that contract. |
| `parallel-backend` | `"threads"` \| `"interpreters"` | Execution backend baked into the generated `typhon_runtime/parallel.py` at build time. `"threads"` (default) uses a `concurrent.futures.ThreadPoolExecutor` (order-preserving; escapes the GIL on a free-threaded build). `"interpreters"` first tries a PEP 734 `concurrent.futures.InterpreterPoolExecutor` (Python 3.14+) and **falls back transparently** to the thread pool on `ImportError` / `AttributeError` (older runtimes) or when the mapped function can't be pickled across the interpreter boundary — the helper probes `pickle.dumps(fn)` before creating a pool, so unshareable callables (whose pickling raises `PicklingError` / `AttributeError`, not just `TypeError`) fall back instead of crashing, while exceptions raised *by* the mapped function still propagate normally. Order is preserved on every path. **Note:** the lambdas the auto-parallel rewrites emit never pickle, so rewritten call sites always run on the thread pool under this backend today — the interpreters pool benefits hand-written `map_pure` calls passing top-level named functions. Any other value is rejected at config load rather than silently falling back to threads. |
| `pgo-memoise` | bool | When `true`, `tyc build` reads `typhon-profile.json` (produced by a prior `tyc profile` run) and promotes every pure function whose observed call count meets `pgo-min-calls` to `@functools.cache`, even if the user did not write `@memo`. Complements `auto-memoise` (which caches every pure function regardless of profile data). Missing profile file is not an error — PGO is best-effort. Default `false` (defaults to `true` at `[optimise] level = 1`; explicit entry wins). |
| `pgo-min-calls` | int | Minimum observed call count for a function to be promoted by `pgo-memoise`. Default `100` — high enough that one-off entry points stay un-cached, low enough that an inner-loop helper qualifies after a single representative run. |
| `suggest-perf` | bool | When `true` (the default), surface the `tyc::perf_*` advice-lint family — micro-optimisation nudges in hot paths. Advice-only; never blocks a build. Set `false` to silence the whole family. |
| `suggest-parallel` | bool | When `true` (the default), surface the two free-threading advice lints: `tyc::parallel_opportunity` (a comprehension / accumulator loop that could be parallelised, or a `float` accumulator ineligible only because of float reordering) and `tyc::shared_mut_across_tasks` (a `go`-spawned same-module function that writes a `global` or module-level `mut` binding). **Both only fire when `[python] free-threaded = true`** — a GIL-target project sees neither. Advice-only; never blocks a build. Set `false` to silence them. |
| `stub-check` | `"error"` \| `"warn"` \| `"off"` | Severity for `tyc::stub_mismatch` produced by `tyc check --stubs`. `"error"` (default) breaks CI on stub drift. `"warn"` surfaces drift without blocking merges. `"off"` silently drops stub mismatches — useful when running `--stubs` opportunistically. |
| `unintrospectable-dependency` | `"warn"` \| `"error"` \| `"off"` | Severity when a declared dependency is imported but its signatures can't be recovered (no reachable `.venv`/`python3`, the package isn't installed, or it exposes no introspectable signatures) — so its third-party arity/type checks are skipped. `"warn"` (default) surfaces the skipped coverage; `"error"` fails the build/check (CI-gating); `"off"` restores the prior silent behaviour. Install the project's dependencies (`uv sync`) or ship a `.dty` stub to clear it. |
| `allow-secret-comptime` | bool | When `true`, silences `tyc::contains_secret_literal` — the warning that fires when a `comptime let` binding whose name looks like a secret (`*KEY` / `*TOKEN` / `*PASSWORD` / `*SECRET` / `*PASS` / `*PWD`) would inline the resolved env-var value as a string literal into the build artifact. Default `false`; `tyc init` seeds it explicitly. Prefer reading secrets at runtime via `os.environ[...]` over enabling this. |

> **`TYC_NO_INTROSPECT=1`** (v1.0.0-alpha.3) — an environment kill-switch that disables venv
> dependency introspection entirely in `tyc check` / `tyc build` / the LSP. Third-party calls
> then degrade to a permissive `Unknown` (their arity/type checks are skipped, and the
> `unintrospectable-dependency` warning is suppressed). It exists for the "opening a project
> imports its dependencies" trust boundary — see [`SECURITY.md`](../SECURITY.md).

### `[env]`

| Key | Type | Description |
|-----|------|-------------|
| `required` | list of strings | Environment variables that **must** resolve at build time. Any `comptime env()` lookup on a missing required variable fails the build. |

### `[checker]`

A second-stage type checker that runs over the **emitted Python** after a
successful `tyc build`, complementing `tyc-types`'s Typhon-specific rules.
This is the only path that type-checks against **typeshed**, so it covers
C-extension and stdlib APIs (numpy, pandas, `os.path`, …) that runtime venv
introspection can't model. See [`ty-integration.md`](ty-integration.md).

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `external` | string | `"none"` | `"ty"` runs Astral's [`ty`](https://github.com/astral-sh/ty) (`ty check`) over the build output and re-attributes its diagnostics back to the `.ty` source via the `.py.map` sidecars. `ty` errors fail the build. Requires `ty` on `PATH` (`pip install ty` / `uv tool install ty`). |
| `external-args` | list of strings | `[]` | Extra arguments forwarded verbatim to the external checker. |

```toml
[checker]
external = "ty"
external-args = []
```

> `ty` is most useful with the project's dependencies installed in a venv,
> so it can resolve third-party imports and their typeshed stubs.

`external = "ty"` spawns the `ty` CLI as a subprocess and re-attributes its
diagnostics to the `.ty` source. (An embedded, in-process variant was
prototyped — see [`ty-integration.md`](ty-integration.md) — but isn't shipped:
it needs a git dependency on `astral-sh/ruff`, which the repo's `cargo deny`
policy disallows, and offers no capability the subprocess path lacks.)
