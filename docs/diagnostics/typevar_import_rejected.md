# tyc::typevar_import_rejected

Fires when a module writes `from typing import TypeVar`. Typhon uses PEP 695
type-parameter syntax (`def f[T](x: T) -> T:` and `class Box[T]:`) and the
`TypeVar(...)` constructor is not a supported value.

## Example

```ty
from typing import TypeVar  # error: TypeVar is not supported
T = TypeVar("T")

def first(xs: list[T]) -> T:
    return xs[0]
```

## Why

PEP 695 makes type parameters first-class syntax instead of an out-of-band
runtime call. The two forms can't comfortably coexist in one type system —
the PEP 695 form is the supported one, and the legacy `TypeVar` import is
rejected to keep the rules consistent.

## Fix

Drop the import and declare the type parameter on the function or class
directly:

```ty
def first[T](xs: list[T]) -> T:
    return xs[0]
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/typevar_import_rejected.md
