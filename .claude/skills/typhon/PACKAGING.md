# Typhon Packaging — Multi-File Projects

Single-file `.ty` programs work fine, but real projects span multiple modules and packages. This file covers the packaging surface: project layout, `__init__.ty`, the `pub` keyword, `pub *` aggregation, import semantics, `.py` interop, and CI integration.

For the language inside any single file, see [SKILL.md](SKILL.md) and [REFERENCE.md](REFERENCE.md).

---

## 1. Project layout

`tyc init NAME` scaffolds:

```
NAME/
├── typhon.toml      # configured by [project] src=src, out=build
├── src/
│   └── main.ty      # canonical "Hello, world" with frozen dataclass + impl + Result/?
└── tests/
```

`[project] src` defines the source root; `[project] out` defines where emitted Python lands (`build/` by default). Tests can live anywhere, but `src/` is what `tyc check` and `tyc build` walk.

For larger projects, group `.ty` files into subdirectories under `src/`:

```
src/
├── main.ty
├── domain/
│   ├── __init__.ty
│   ├── ids.ty           # newtype UserId = int, etc.
│   ├── user.ty          # class User: ...
│   └── post.ty
├── storage/
│   ├── __init__.ty
│   ├── sqlite.ty
│   └── memory.ty
├── transport/
│   ├── __init__.ty
│   ├── http.ty
│   └── grpc.ty
└── stubs/
    ├── redis.dty
    └── third_party_lib.dty
```

`tyc build` mirrors the source tree into `build/`: `src/domain/user.ty` → `build/domain/user.py`. Subpackage `__init__.ty` files generate `build/domain/__init__.py`.

See `examples/apps/01..15-*/` for canonical multi-file layouts.

---

## 2. Imports

Typhon's import syntax matches Python's (it IS Python at the parser level), but the resolver is stricter.

```python
# Absolute imports — bare module name resolves under src/
from domain.user import User
import storage.sqlite as sql

# Relative imports — for intra-package use
from .ids import UserId
from ..storage import open_store

# Standard library
import os
import asyncio
from typing import Callable
```

Constraints:

- `tyc::unknown_module` fires when `import X` names a module not in: Python stdlib, the project tree, `typhon_runtime`, or `[dependencies]` declared in `typhon.toml`.
- `tyc::unused_import` fires (severity `error` by default) when an imported name is never referenced. Suppress by removing or by giving the binding a leading underscore (side-effect-only imports). The LSP's "Remove unused import" code action handles it.
- `tyc::orphan_py_import` (warn) fires when a relative `.py` import resolves outside `src/`. `tyc build` only copies files under `src/` into the output, so the emitted Python would crash with `ModuleNotFoundError`. Move under `src/` or use an absolute import.
- `tyc::stdlib_module_shadow` (warn, v0.6.0 + v0.7.0 refinement) fires when a top-level `.ty` file's stem matches a Python 3.13 stdlib top-level module name. The emitted `build/<name>.py` would land on `sys.path` and intercept transitive stdlib imports. **Only fires for files at the top of the configured source directory** (v0.7.0); nested files like `src/indexer/tokenize.ty` are exempt because they lower to `build/indexer/tokenize.py` which is NOT on `sys.path`. Rename top-level files (e.g. `lang_types.ty`, `records.ty`).
- `tyc::typevar_import_rejected` blocks `from typing import TypeVar` — use PEP 695 (`def f[T](...)`).
- `tyc::typing_alias_deprecated` blocks `from typing import List/Dict/Tuple/...` — use lowercase built-ins.

`from X import Y` inside `if`/`for`/`while`/`with`/`try`/`match` arms binds (v0.7.0). Use this for conditional imports:

```python
if sys.platform == "darwin":
    from .mac_helpers import platform_call
else:
    from .linux_helpers import platform_call
```

---

## 3. The `pub` visibility modifier

`pub` marks a top-level declaration as part of the public API. When a module declares at least one `pub` name, desugar synthesises a top-of-file `__all__ = [...]` in source order.

```python
# src/domain/user.ty
pub class User:
    id: int
    name: str
    email: str

pub def make_user(name: str, email: str) -> User: ...

pub let DEFAULT_DOMAIN: str = "example.com"

let _internal_default_port: int = 8080   # not exported (no `pub`)
```

Emits:

```python
__all__ = ["User", "make_user", "DEFAULT_DOMAIN"]

from dataclasses import dataclass

@dataclass(slots=True)
class User:
    id: int
    name: str
    email: str

def make_user(name: str, email: str) -> User: ...

DEFAULT_DOMAIN: str = "example.com"

_internal_default_port: int = 8080
```

If no `pub` exists in a module, no `__all__` is emitted — the module behaves like a normal Python file with no explicit public surface. A hand-written `__all__` wins over the synthesised one.

`pub` stacks with every modifier keyword:

- `pub let`, `pub mut`
- `pub freeze let` (v0.6.0)
- `pub def`, `pub async def`
- `pub class`, `pub frozen class`, `pub class!`, `pub plain class`
- `pub model`
- `pub interface`
- `pub newtype`
- `pub type`

The Round-3 finding R2-1 closed in v0.6.0: `pub def f(...) -> Result[T, E]:` is now visible to the `?` operator validator.

---

## 4. `pub *` — package-level re-export aggregation (v0.7.0)

In an `__init__.ty`, the single statement `pub *` aggregates every direct-sibling module's `pub`-marked declarations and, transitively, every direct sub-package's effective public surface.

```python
# src/mypkg/__init__.ty
pub *
```

Suppose:

```
src/mypkg/
├── __init__.ty           # pub *
├── ids.ty                # pub newtype UserId = int; pub newtype PostId = int
├── user.ty               # pub class User: ...; pub def make_user(...) -> User: ...
└── post.ty               # pub class Post: ...
```

`tyc build` emits roughly:

```python
# build/mypkg/__init__.py
from .ids import UserId, PostId
from .user import User, make_user
from .post import Post

__all__ = ["UserId", "PostId", "User", "make_user", "Post"]
```

Sibling order is alphabetical by basename. Sub-packages are included **transitively** — when a direct sub-directory contains its own `__init__.ty`, the sub-package's effective public surface (its own `pub`-marked names, plus whatever its own `pub *` aggregates one level deeper) is re-exported. Recursion is cycle-safe via a `visited` set keyed on each package directory.

The `pub *` marker is preserved on a single line so source maps stay byte-aligned.

### Mixed `pub` + `pub *`

A package can also declare its own `pub` names in `__init__.ty`. Those wins on collision with sibling exports:

```python
# src/mypkg/__init__.ty
pub *
pub let VERSION: str = "1.0.0"     # package-level export, overrides any sibling
```

### Diagnostics

- **`tyc::pub_name_collision`** (error): two siblings both `pub`-export the same name. Names both modules and the colliding name. Fix: rename, drop the `pub` on one, or replace `pub *` with an explicit `from .module import …` list.
- **`tyc::pub_star_outside_init`** (advice): `pub *` in a non-`__init__.ty` module is a no-op with confusing intent. Move to `__init__.ty` or remove. Fires from both `tyc check` and `tyc build` so CI surfaces the dead marker.

### Why `pub *` and not just hand-written re-exports?

Without `pub *`, every time you add a `pub class` to a sibling module you'd have to remember to re-add it to `__init__.ty`'s `from .sibling import …` list. `pub *` makes the re-export automatic. Drop in a new sibling module; its `pub` names flow up to the package facade with no extra ceremony.

---

## 5. Recommended package shape for non-trivial apps

The `examples/apps/01..15-*/` projects use this layout consistently:

```
src/
├── __init__.ty          # pub * (or empty if main.ty is the entry)
├── main.ty              # entry point
├── domain/
│   ├── __init__.ty      # pub *
│   ├── ids.ty           # newtype declarations
│   ├── events.ty        # sealed-union event types
│   └── ...              # domain types and helpers
├── runtime/             # core processing logic
│   ├── __init__.ty      # pub *
│   ├── engine.ty
│   └── ...
├── storage/             # persistence layer
│   ├── __init__.ty      # pub *
│   ├── sqlite.ty
│   └── ...
├── transport/           # HTTP / gRPC / WebSocket
│   ├── __init__.ty      # pub *
│   ├── http.ty
│   └── ...
└── stubs/               # .dty for third-party libs
    └── third_party.dty
```

Cross-package imports stay short (`from domain import EntityId`); intra-package imports use relative form (`from .ids import EntityId`).

---

## 6. `.dty` stubs

`.dty` is Typhon's stub format. See [RUNTIME.md](RUNTIME.md) §4 for the full workflow. Brief:

- One `.dty` per library you want strictly typed. Place them under `src/stubs/` (or anywhere under `src/`).
- Bodies omitted; signatures + class/method declarations + annotated fields only.
- `tyc build` emits `.pyi` companions for every `.dty`.
- `tyc check --stubs` and `tyc stubtest` validate stubs against the runtime they describe.

A `.pyi` consumed *by* Typhon is treated as an `unsafe` boundary unless an authored `.dty` overrides it.

```python
# src/stubs/redis.dty
class Redis:
    host: str
    port: int

impl Redis:
    def get(self, key: str) -> str?
    def set(self, key: str, value: str) -> bool
    def delete(self, *keys: str) -> int
```

---

## 7. Dependencies

`[dependencies]` and `[dev-dependencies]` in `typhon.toml` are managed via `tyc add` / `tyc remove` / `tyc sync` — thin wrappers over `uv`.

```toml
[dependencies]
requests = ">=2.31"
rich = "*"                       # bare name → any version
pydantic = ">=2.0"

[dev-dependencies]
pytest = "8.2"                   # bare version → ==8.2
```

```bash
tyc add requests              # rewrites typhon.toml + uv sync
tyc add --dev pytest@8.2      # under [dev-dependencies]
tyc add --no-sync foo bar     # batch edits; finish with `tyc sync`
tyc remove rich
tyc sync                      # write pyproject.toml + uv sync
tyc sync --dry-run            # preview the generated pyproject.toml
```

`tyc build` merges owned keys (`[project] name/version/requires-python/dependencies`, `[dependency-groups].dev`) into a generated `pyproject.toml` at the project root. User-managed `[tool.*]` tables and other `[project]` keys are preserved byte-for-byte.

If `uv` is missing on PATH, `tyc add` still rewrites the manifest and prints an "install uv" message. `tyc build --no-sync` (or `TYC_NO_SYNC=1`) skips `uv sync` entirely.

---

## 8. Tests

Tests use the same `.ty` extension and run under the configured Python interpreter. Pytest is the default test runner.

```
tests/
└── test_calculator.ty
```

```python
import pytest

from src.calculator import div, average


def test_div_by_zero() -> None:
    match div(1.0, 0.0):
        case Ok(_):
            pytest.fail("expected Err")
        case Err(e):
            assert isinstance(e, DivideByZero)


@pytest.mark.parametrize("xs,expected", [
    ([1.0], 1.0),
    ([1.0, 3.0], 2.0),
])
def test_average_parametrised(xs: list[float], expected: float) -> None:
    match average(xs):
        case Ok(v):
            assert v == expected
        case Err(_):
            pytest.fail("unexpected Err")
```

After `tyc build`, run pytest as usual against the emitted Python:

```bash
tyc build
pytest build/tests/    # or wherever your tests landed
```

`examples/testing/` is the canonical pattern.

---

## 9. CI integration

```yaml
- run: tyc check src/                  # primary gate; non-zero on any error severity
- run: tyc check --stubs               # if you ship .dty stubs
- run: tyc build                       # full pipeline (optional in CI; check covers most)
- run: tyc ty                          # second-opinion (needs `pip install ty`)
- run: tyc stubtest                    # runtime stub probe (needs mypy in the venv)
- run: pytest build/                   # if you ship tests
```

`tyc check` exits non-zero on any `tyc::*` diagnostic at `"error"` severity, plus on missing required env vars (`comptime let` reads listed in `[env] required`).

Severity controls (in `typhon.toml`):

```toml
[strictness]
unused-import          = "error"  # default
exhaustive-match       = "error"  # default
methods-in-class-body  = "warn"   # bump to "error" to break CI on Rule 4 violations
require-with           = "warn"   # bump to "error" for tyc::resource_not_managed
blocking-in-async      = "warn"   # bump to "error" for tyc::blocking_in_async
stub-check             = "error"  # default
```

---

## 10. Build artifacts

After `tyc build`:

```
build/
├── main.py
├── typhon_runtime/          # only if needed (Result, go, lazy let, etc.)
│   ├── __init__.py
│   ├── result.py
│   ├── tasks.py
│   ├── lazy.py
│   ├── parallel.py
│   ├── freeze.py
│   └── stdlib.py
├── domain/
│   ├── __init__.py
│   ├── ids.py
│   ├── ids.pyi          # if domain/ids.dty exists
│   └── ...
├── .sourcemaps/         # v0.6.1+ — .py.map sidecars live here
│   ├── main.py.map
│   ├── domain/
│   │   ├── ids.py.map
│   │   └── ...
│   └── ...
└── pyproject.toml       # generated; merged with user-managed [tool.*] tables
```

The `build/` tree is the deployable artifact. **It contains zero Typhon-specific dependencies** — just CPython code plus the generated `typhon_runtime/` package (no PyPI release of that package; every project ships its own copy).

### Don't commit `build/` to source control

Add to `.gitignore`:

```
build/
typhon-profile.json
```

`tyc build` regenerates everything from `src/`.

---

## 11. `__init__.ty` cookbook

### Minimal package facade

```python
# src/mypkg/__init__.ty
pub *
```

### Package with its own exports + sibling aggregation

```python
# src/mypkg/__init__.ty
pub *

pub let VERSION: str = "1.0.0"

pub def configure(level: str) -> None:
    logging.basicConfig(level=level.upper())
```

### Package with explicit exports (no `pub *`)

When you want fine-grained control:

```python
# src/mypkg/__init__.ty
from .ids import UserId, PostId
from .user import User
from .post import Post

__all__ = ["UserId", "PostId", "User", "Post"]
```

A hand-written `__all__` wins over the auto-synthesised one — useful when you want to expose a curated subset.

### Empty `__init__.ty` for a namespace package

```python
# src/mypkg/__init__.ty
```

Or just omit the file entirely — Python's PEP 420 implicit namespace packages work fine.

---

## 12. Common packaging pitfalls

1. **Top-level filename matches a Python stdlib module.** `src/types.ty` lowers to `build/types.py` which lands on `sys.path` and shadows `import types` everywhere. Rename. (`tyc::stdlib_module_shadow`)
2. **Relative `.py` import to a file outside `src/`.** `tyc build` only copies `src/` to `build/` — the missing `.py` causes `ModuleNotFoundError` at runtime. (`tyc::orphan_py_import`)
3. **Two siblings both `pub def hello`.** `pub *` aggregation gets ambiguous. Rename one, drop `pub` on one, or use explicit re-exports. (`tyc::pub_name_collision`)
4. **`pub *` in a non-`__init__.ty` file.** No-op with confusing intent. Move to `__init__.ty`. (`tyc::pub_star_outside_init`)
5. **Importing `typing.TypeVar` / `typing.List` / etc.** Rejected. Use PEP 695 brackets and lowercase built-ins.
6. **Forgetting to `tyc sync` after `tyc add`.** The manifest updates but the venv doesn't. Either let `tyc add` sync (default) or run `tyc sync` explicitly.
7. **Committing `build/` to source control.** Generated artifact; regenerate on each build. Add to `.gitignore`.
8. **Editing `typhon_runtime/`.** Regenerated on every `tyc build` — your edits will be overwritten. If you need different runtime behaviour, file an issue.
