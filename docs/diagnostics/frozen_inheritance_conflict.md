# tyc::frozen_inheritance_conflict

Fires when a dataclass and its in-module dataclass base disagree on
frozen-ness: a `frozen` class inheriting a non-frozen one, or a non-frozen
class inheriting a `frozen` one.

## Example

```ty
class Shape:
    name: str

class Square frozen(Shape):   # error: frozen child of a non-frozen base
    side: float
```

The reverse is equally rejected:

```ty
class Base frozen:
    name: str

class Derived(Base):          # error: non-frozen child of a frozen base
    extra: int
```

## Why

A Typhon `class` compiles to a `@dataclasses.dataclass`. CPython refuses to
build a frozen dataclass that inherits a non-frozen one — or a non-frozen one
that inherits a frozen base — and raises `TypeError` at class-definition
(import) time:

```
TypeError: cannot inherit frozen dataclass from a non-frozen one
```

Without this check the program type-checked cleanly but the emitted module
crashed the moment it was imported. Typhon rejects it up front instead.

Only in-module dataclass bases are compared. Inheriting an external or
non-dataclass base (an `Enum`, a framework base class, …) is unaffected —
CPython only forbids the disagreement between two dataclasses.

## Fix

Make the child and its base agree — both `frozen` or neither:

```ty
class Shape frozen:
    name: str

class Square frozen(Shape):   # ok: both frozen
    side: float
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/frozen_inheritance_conflict.md
