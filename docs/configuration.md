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
skip-decoration-bases = []  # extra base classes that suppress @dataclass injection
model-extra = "forbid"      # ConfigDict(extra=…) for model classes: "forbid" | "ignore" | "allow"

[strictness]
no-implicit-any = true
unused-import = "error"
exhaustive-match = "error"
methods-in-class-body = "warn"  # severity for Rule-4 violations (methods belong in `impl Name:`)
auto-memoise = false        # opt-in: insert @functools.cache on inferred-pure functions
auto-gather = false         # opt-in: fold straight-line independent `await` runs into TaskGroup
auto-parallel = false       # opt-in: rewrite pure list comprehensions to a thread-pool map
parallel-min-size = 64      # minimum iterable size for auto-parallel to fire
pgo-memoise = false         # opt-in: promote hot pure fns to @functools.cache from typhon-profile.json
pgo-min-calls = 100         # minimum profile-recorded call count for pgo-memoise to fire
stub-check = "error"        # severity for tyc::stub_mismatch from `tyc check --stubs`

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
| `target` | `"3.13"` \| `"3.13t"` \| `"3.14"` \| `"3.14t"` | Target CPython version. **Typhon requires 3.13+**; older targets are rejected at config load with `unsupported [python] target` and an actionable error. The `t` suffix selects free-threaded emission paths (PEP 703); see `free-threaded` below. |
| `free-threaded` | bool | Allow free-threaded emission paths. Default `false` until 3.14 is the default Python. Use a `t` suffix on `target` to attest at config time. |

### `[emit]`

| Key | Type | Description |
|-----|------|-------------|
| `class-default` | `"dataclass"` \| `"pydantic"` | Default emit target for `class` declarations. Overridable per-class via the `model` keyword. Unknown values (`"struct"`, `"plain"`, `"none"`, the empty string, …) are rejected at config load with `tyc::invalid_config_value` rather than silently treated as `"dataclass"`. |
| `format` | bool | Post-process emitted `.py` through `ruff format`. |
| `skip-decoration-bases` | list of strings | Extra base-class names that suppress the automatic `@dataclasses.dataclass(slots=True)` decorator and trigger plain-class emission with a synthesised `__init__` calling `super().__init__()`. Matched by *last segment*, so `["BaseModel", "MyCustomBase"]` catches `pydantic.BaseModel` and `mylib.frameworks.MyCustomBase` regardless of how they're imported. Built-in entries (`Protocol`, `Enum`, `IntEnum`, `Flag`, `IntFlag`, `StrEnum`, `ABC`, `NamedTuple`, `BaseModel`, `App`) are auto-skipped without needing to be listed. |
| `model-extra` | `"forbid"` \| `"ignore"` \| `"allow"` | Value for `ConfigDict(extra=…)` injected into every `model` class. Default `"forbid"` rejects unexpected fields at runtime. `"ignore"` silently drops them; `"allow"` passes them through as extra attributes. Unknown values are rejected at config load. |

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

### `[strictness]`

| Key | Type | Description |
|-----|------|-------------|
| `no-implicit-any` | bool | Reserved — surfaces today as the `tyc::missing_annotation` diagnostic on un-annotated parameters and return types. The flag is parsed for forward compatibility; toggling it does not currently relax the check. |
| `unused-import` | `"error"` \| `"warn"` \| `"off"` | Severity for unused imports. |
| `exhaustive-match` | `"error"` \| `"warn"` \| `"off"` | Severity for non-exhaustive `match` over sealed unions. |
| `methods-in-class-body` | `"error"` \| `"warn"` \| `"off"` | Severity for `tyc::method_in_class_body` (Rule 4: methods live in `impl Name:`, not the class body). Default `"warn"` matches every other nudge diagnostic. Promote to `"error"` to break CI once your codebase has migrated. `"off"` suppresses the diagnostic entirely — useful for codebases still mid-migration. |
| `auto-memoise` | bool | Whether to apply `@functools.cache` to functions the analyser infers as pure. Default `false`. Caches are *never* inserted silently: even when enabled, the analyser requires all six purity conditions (see [language.md](language.md)). |
| `auto-gather` | bool | When `true`, runs of two-or-more consecutive independent `name = await callee(...)` statements inside an `async def` are folded into an `asyncio.TaskGroup` so they execute concurrently. Independence is decided by static data-flow on bound names; **every callee must be a same-module `async def` carrying an explicit `@gatherable` decorator** — undecorated async functions and externally-imported callees are left alone, so opting a project into `auto-gather` does not surprise callers that aren't ready to run concurrently. Default `false`. Explicit `gather:` blocks are unaffected. |
| `auto-parallel` | bool | When `true`, list comprehensions whose element is a pure call are rewritten at build time into a thread-pool map. Combine with `[python] free-threaded` to release the GIL across workers; on stock CPython the rewrite still runs but the GIL serialises workers. Default `false`. |
| `parallel-min-size` | int | Minimum statically-detectable iterable length for a comprehension to qualify for `auto-parallel`. Default `64`. When the iterable size cannot be inferred, the threshold is treated as zero — users opting in accept that contract. |
| `pgo-memoise` | bool | When `true`, `tyc build` reads `typhon-profile.json` (produced by a prior `tyc profile` run) and promotes every pure function whose observed call count meets `pgo-min-calls` to `@functools.cache`, even if the user did not write `@memo`. Complements `auto-memoise` (which caches every pure function regardless of profile data). Missing profile file is not an error — PGO is best-effort. Default `false`. |
| `pgo-min-calls` | int | Minimum observed call count for a function to be promoted by `pgo-memoise`. Default `100` — high enough that one-off entry points stay un-cached, low enough that an inner-loop helper qualifies after a single representative run. |
| `stub-check` | `"error"` \| `"warn"` \| `"off"` | Severity for `tyc::stub_mismatch` produced by `tyc check --stubs`. `"error"` (default) breaks CI on stub drift. `"warn"` surfaces drift without blocking merges. `"off"` silently drops stub mismatches — useful when running `--stubs` opportunistically. |
| `unintrospectable-dependency` | `"warn"` \| `"error"` \| `"off"` | Severity when a declared dependency is imported but its signatures can't be recovered (no reachable `.venv`/`python3`, the package isn't installed, or it exposes no introspectable signatures) — so its third-party arity/type checks are skipped. `"warn"` (default) surfaces the skipped coverage; `"error"` fails the build/check (CI-gating); `"off"` restores the prior silent behaviour. Install the project's dependencies (`uv sync`) or ship a `.dty` stub to clear it. |

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
