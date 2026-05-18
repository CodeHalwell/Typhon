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
model-extra = "forbid"      # "forbid" | "allow" | "ignore" — passes to Pydantic ConfigDict
format = true               # post-process through ruff format
pyi-stubs = true            # emit .pyi alongside .py for interop with mypy / pyright / Pyrefly / ty

[strictness]
no-implicit-any = true
unused-import = "error"
exhaustive-match = "error"
auto-memoise = false        # opt-in: insert @functools.cache on inferred-pure functions
auto-parallel = false       # opt-in: rewrite pure list comprehensions to a thread-pool map
parallel-min-size = 64      # minimum iterable size for auto-parallel to fire
stub-check = "error"        # tyc check --stubs severity; compares .pyi against runtime modules

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
| `model-extra` | `"forbid"` \| `"allow"` \| `"ignore"` | Value passed to Pydantic's `ConfigDict(extra=...)` for `model` emissions. Default `"forbid"` — Typhon does not inherit Pydantic's stock `"ignore"` because silently dropping input contradicts the safety pitch. |
| `format` | bool | Post-process emitted `.py` through `ruff format`. |
| `pyi-stubs` | bool | Emit a PEP 561 `.pyi` next to every `.py`. Default `true`. Disable for projects that vendor stubs separately. |

### `[strictness]`

| Key | Type | Description |
|-----|------|-------------|
| `no-implicit-any` | bool | Treat implicit `Any` as a hard error outside `unsafe` blocks. |
| `unused-import` | `"error"` \| `"warn"` \| `"off"` | Severity for unused imports. |
| `exhaustive-match` | `"error"` \| `"warn"` \| `"off"` | Severity for non-exhaustive `match` over sealed unions. |
| `auto-memoise` | bool | Whether to apply `@functools.cache` to functions the analyser infers as pure. Default `false`. Caches are *never* inserted silently: even when enabled, the analyser requires all six purity conditions (see [language.md](language.md)). |
| `stub-check` | `"error"` \| `"warn"` \| `"off"` | Severity for drift between a `.dty` source and the runtime module it describes. Surfaced by `tyc check --stubs`. |

### `[env]`

| Key | Type | Description |
|-----|------|-------------|
| `required` | list of strings | Environment variables that **must** resolve at build time. Any `comptime env()` lookup on a missing required variable fails the build. |
