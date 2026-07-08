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

## Alternative: nullary sealed-union variants

If you're declaring nullary variants of a sealed union (R2-2 in
apps-feedback), you do not need the `placeholder: int = 0` workaround any
more — `pass` is accepted on a `frozen` class body:

```ty
pub class TyInt frozen:
    pass

pub class TyFloat frozen:
    pass

pub class TyStr frozen:
    pass

pub type Ty = TyInt | TyFloat | TyStr
```

Drop the placeholder fields and the warning disappears. Construction stays
nullary: `TyInt()`, `TyFloat()`, `TyStr()`.

## Mutable-default carve-out (v0.9.0)

Since v0.9.0 the warning no longer false-positives on classes whose
only annotated defaults are mutable literals (`list[str] = []`,
`dict[str, int] = {}`, `set[int] = set()`). Those defaults are
rewritten at desugar time into `default_factory` calls, so each
instance gets its own list/dict/set rather than sharing a single
constant — and the warning's "looks like a constants namespace"
heuristic no longer applies.

```ty
class Bucket:
    items: list[str] = []   # ✓ no warning since v0.9.0 — per-instance factory
```

The warning still fires on immutable literals (`int = 3`, `str = "x"`),
where the slot-descriptor pitfall actually applies.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/class_attr_shadows_slot.md
