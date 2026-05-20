# tyc::manual_init

Fires when a `class` body declares `__init__` directly. Typhon generates the
constructor from the class's field annotations, so writing one manually
conflicts with the emitted dataclass / model.

## Example

```ty
class Point:
    x: int
    y: int

    def __init__(self, x: int, y: int) -> None:  # error: manual __init__
        self.x = x
        self.y = y
```

## Why

Typhon's default class emit is `@dataclass(slots=True)`, which already
generates a constructor from the fields. A user-supplied `__init__` either
duplicates or contradicts the generated one, depending on how the decorator
handles the conflict — either way the result is confusing.

## Fix

Remove the manual `__init__` and rely on the generated constructor. For
custom defaults, set them as field defaults; for custom construction logic,
write a free factory function:

```ty
class Point:
    x: int = 0
    y: int = 0

def origin() -> Point:
    return Point()
```

See https://typhon.dev/lang/diagnostics/manual_init
