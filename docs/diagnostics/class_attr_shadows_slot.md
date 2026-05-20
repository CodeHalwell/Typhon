# tyc::class_attr_shadows_slot

Warns when a `class` body contains only annotated assignments with defaults
(`NAME: T = literal`) and no methods or per-instance fields, so it reads like
a namespace of constants. The class emits as `@dataclass(slots=True)`, which
turns each name into a slot descriptor — so `Klass.NAME` at runtime returns
the descriptor, not the literal.

## Example

```ty
class Limits:
    MAX_RETRIES: int = 3  # warning: will be a slot descriptor at runtime
```

## Why

`@dataclass(slots=True)` is Typhon's default class emit mode and excludes
any name listed as a `__slots__` entry from being a normal class attribute.
That makes `Limits.MAX_RETRIES` evaluate to the slot descriptor object, not
to `3`, which silently breaks any code treating the class as a constants
namespace.

## Fix

Annotate each field as `ClassVar[T]` so the dataclass decorator excludes it
from `__slots__`:

```ty
from typing import ClassVar

class Limits:
    MAX_RETRIES: ClassVar[int] = 3
```

See https://typhon.dev/lang/diagnostics/class_attr_shadows_slot
