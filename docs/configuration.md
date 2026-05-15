# `typhon.toml` Reference

> Excerpted from the [long-term plan](long-term-plan.md). The plan is the source of truth.

Each Typhon project has a `typhon.toml` at its root, written by `ttc init` and read by every subcommand. Standard `pip`/`uv` workflows handle dependencies — Typhon does not ship a package manager.

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

[env]
required = ["DATABASE_URL"]  # comptime env() lookups must resolve at build time
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

### `[strictness]`

| Key | Type | Description |
|-----|------|-------------|
| `no-implicit-any` | bool | Treat implicit `Any` as a hard error outside `unsafe` blocks. |
| `unused-import` | `"error"` \| `"warn"` \| `"off"` | Severity for unused imports. |
| `exhaustive-match` | `"error"` \| `"warn"` \| `"off"` | Severity for non-exhaustive `match` over sealed unions. |

### `[env]`

| Key | Type | Description |
|-----|------|-------------|
| `required` | list of strings | Environment variables that **must** resolve at build time. Any `comptime env()` lookup on a missing required variable fails the build. |
