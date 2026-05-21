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

See https://typhon.dev/lang/diagnostics/pub
