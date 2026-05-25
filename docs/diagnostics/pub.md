# pub — public API surface

`pub` is a module-level modifier that marks a declaration as part of
the public API of its module. When at least one `pub` declaration is
present, the desugar pass emits a synthesised `__all__ = [...]` list
at the top of the resulting Python module containing every `pub`
name in source order.

## Example

```ty
pub def greet(name: str) -> str:
    return f"hi {name}"

def _internal_helper() -> int:
    return 42

pub class User:
    name: str
    age: int

pub let API_VERSION: str = "v1"

pub newtype UserId = int
```

Emitted Python:

```python
__all__ = ["greet", "User", "API_VERSION", "UserId"]

def greet(name: str) -> str: ...
def _internal_helper() -> int: ...

@dataclasses.dataclass(slots=True)
class User: ...

API_VERSION: str = "v1"
UserId = NewType("UserId", int)
```

## Why

Python has no language-level concept of public vs. private; the
convention is a leading underscore on private names, but downstream
tooling (`from foo import *`, Sphinx autoapi, IDE re-export
filters, type checker re-export inference) takes its cue from
`__all__` when present. Maintaining `__all__` by hand is tedious and
drifts as the surface grows — `pub` flips the maintenance direction
so the compiler does the bookkeeping.

## Where it applies

`pub` may appear at the start of any module-level declaration:

```ty
pub def fn(...): ...
pub async def fn(...): ...
pub class Foo: ...
pub class Foo frozen: ...
pub class! Foo: ...
pub plain class Foo: ...
pub model Foo: ...
pub interface Foo: ...
pub newtype Foo = int
pub type Foo = int | str
pub let X: int = 1
pub mut Y: int = 0
```

It is **not** valid inside a function body or class body —
encapsulation of fields, locals, and inner classes is a separate
concern and out of scope for the current MVP.

## Hand-written `__all__` wins

If the module already declares its own `__all__ = [...]` (typed or
plain), the desugar pass leaves it untouched even when `pub` names
are present. This is the escape hatch for re-exporting a name that
isn't `pub`-marked in the current module, or trimming the
synthesised list.

## Round-tripping

`tyc fmt` preserves the `pub` prefix on every line where it was
written. The postprocessor restores `pub` last so it ends up at the
front of any other modifier (`pub let`, `pub mut`, `pub model`,
`pub frozen class`, …) on the same line.

## Package-level re-export — `pub *` in `__init__.ty`

By default, `pub` only affects the file it appears in: `pub class
Widget:` in `mypkg/widget.ty` is reachable as `mypkg.widget.Widget`
but not as `mypkg.Widget`. Multi-module packages typically hand-write
`mypkg/__init__.ty` with the re-exports they want.

Place a single `pub *` line in `mypkg/__init__.ty` and the build
pipeline aggregates every sibling module's `pub` names into the
package's emitted `__init__.py`:

```ty
# src/mypkg/__init__.ty
pub *

# Optional: package-level pub names live alongside the aggregated ones.
pub let PACKAGE_VERSION: str = "0.1"
```

```ty
# src/mypkg/widget.ty
pub class Widget:
    name: str

pub def make_widget(n: str) -> Widget:
    return Widget(name=n)
```

```ty
# src/mypkg/util.ty
pub def shout(s: str) -> str:
    return s.upper()
```

Emitted `build/mypkg/__init__.py`:

```python
__all__ = ["PACKAGE_VERSION", "shout", "Widget", "make_widget"]

PACKAGE_VERSION: str = "0.1"

from .util import shout
from .widget import Widget, make_widget
```

Downstream callers can now write `from mypkg import Widget, shout`
without touching `__init__.ty` every time a sibling adds a new `pub`
name.

### Ordering

Sibling submodules are aggregated in alphabetical order by basename so
the emitted `__init__.py` is deterministic across runs and across
platforms (where filesystem ordering varies).

### Name collisions

When two siblings export the same name, the first sibling (in
alphabetical order) wins and the build pipeline emits a
`pub_name_collision` warning telling the user which definition was
kept. To pick the other one explicitly, remove `pub *` and write the
re-exports by hand, or rename one of the colliding declarations.

A name explicitly `pub`-declared in `__init__.ty` itself always
overrides any sibling export of the same name — the package-level
declaration is treated as the canonical definition.

### Scope

- `pub *` is honoured **only** in `__init__.ty`. The marker is
  parsed and stripped in every `.ty` file, but outside `__init__.ty`
  it has no effect and the build pipeline emits
  `tyc::pub_star_outside_init` pointing at the wrong-place use.
- Aggregation is **transitive across sub-packages**. Direct-sibling
  `.ty` modules contribute their top-level `pub` names. A direct
  sub-directory with its own `__init__.ty` contributes its effective
  public surface — its own `pub` names plus, recursively, whatever
  its own `pub *` aggregates one level deeper. The recursion is
  cycle-safe via a `visited` set keyed on each package directory, so
  pathological repository layouts (a sub-package whose `__init__.ty`
  somehow re-enters the parent) cannot loop.
- The marker is **opt-in**. Packages without `pub *` keep the
  previous (explicit) behaviour: `__init__.ty` is emitted unchanged
  and only re-exports what the author wrote.

See https://typhon.dev/lang/diagnostics/pub
