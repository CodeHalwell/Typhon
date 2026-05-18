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
target = "3.13"             # or "3.14"
free-threaded = false       # opt-in; requires 3.13t/3.14t

[emit]
class-default = "dataclass" # or "pydantic"
format = true               # post-process through ruff format

[strictness]
no-implicit-any = true
unused-import = "error"
exhaustive-match = "error"
auto-memoise = false        # opt-in: insert @functools.cache on inferred-pure functions
auto-gather = false         # opt-in: fold straight-line independent `await` runs into TaskGroup
auto-parallel = false       # opt-in: rewrite pure list comprehensions to a thread-pool map
parallel-min-size = 64      # minimum iterable size for auto-parallel to fire
pgo-memoise = false         # opt-in: promote hot pure fns to @functools.cache from typhon-profile.json
pgo-min-calls = 100         # minimum profile-recorded call count for pgo-memoise to fire

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
| `target` | `"3.13"` \| `"3.14"` | Target CPython version. |
| `free-threaded` | bool | Allow free-threaded emission paths. Default `false` until 3.14 is the default Python. |

### `[emit]`

| Key | Type | Description |
|-----|------|-------------|
| `class-default` | `"dataclass"` \| `"pydantic"` | Default emit target for `class` declarations. Overridable per-class via the `model` keyword. |
| `format` | bool | Post-process emitted `.py` through `ruff format`. |

> **Always-on behaviour.** Two emit policies that used to be exposed in
> `typhon.toml` are no longer configurable and run unconditionally:
>
> - **PEP 561 `.pyi` stubs** are emitted for every `.dty` file next to the
>   project, so mypy / pyright / Pyrefly / `ty` can consume Typhon-authored
>   libraries without an interop tax. The previously-documented
>   `[emit] pyi-stubs` toggle has been removed.
> - **Pydantic `model` classes** are always emitted with
>   `model_config = ConfigDict(extra="forbid")` — the safety pitch
>   forbids silently dropping unexpected input. A configurable
>   `model-extra` knob is on the roadmap but not yet wired in.

### `[strictness]`

| Key | Type | Description |
|-----|------|-------------|
| `no-implicit-any` | bool | Treat implicit `Any` as a hard error outside `unsafe` blocks. |
| `unused-import` | `"error"` \| `"warn"` \| `"off"` | Severity for unused imports. |
| `exhaustive-match` | `"error"` \| `"warn"` \| `"off"` | Severity for non-exhaustive `match` over sealed unions. |
| `auto-memoise` | bool | Whether to apply `@functools.cache` to functions the analyser infers as pure. Default `false`. Caches are *never* inserted silently: even when enabled, the analyser requires all six purity conditions (see [language.md](language.md)). |
| `auto-gather` | bool | When `true`, runs of two-or-more consecutive independent `name = await callee(...)` statements inside an `async def` are folded into an `asyncio.TaskGroup` so they execute concurrently. Independence is decided by static data-flow on bound names; **every callee must be a same-module `async def` carrying an explicit `@gatherable` decorator** — undecorated async functions and externally-imported callees are left alone, so opting a project into `auto-gather` does not surprise callers that aren't ready to run concurrently. Default `false`. Explicit `gather:` blocks are unaffected. |
| `auto-parallel` | bool | When `true`, list comprehensions whose element is a pure call are rewritten at build time into a thread-pool map. Combine with `[python] free-threaded` to release the GIL across workers; on stock CPython the rewrite still runs but the GIL serialises workers. Default `false`. |
| `parallel-min-size` | int | Minimum statically-detectable iterable length for a comprehension to qualify for `auto-parallel`. Default `64`. When the iterable size cannot be inferred, the threshold is treated as zero — users opting in accept that contract. |
| `pgo-memoise` | bool | When `true`, `tyc build` reads `typhon-profile.json` (produced by a prior `tyc profile` run) and promotes every pure function whose observed call count meets `pgo-min-calls` to `@functools.cache`, even if the user did not write `@memo`. Complements `auto-memoise` (which caches every pure function regardless of profile data). Missing profile file is not an error — PGO is best-effort. Default `false`. |
| `pgo-min-calls` | int | Minimum observed call count for a function to be promoted by `pgo-memoise`. Default `100` — high enough that one-off entry points stay un-cached, low enough that an inner-loop helper qualifies after a single representative run. |

### `[env]`

| Key | Type | Description |
|-----|------|-------------|
| `required` | list of strings | Environment variables that **must** resolve at build time. Any `comptime env()` lookup on a missing required variable fails the build. |
