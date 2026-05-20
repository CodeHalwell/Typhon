# tyc::method_in_class_body

Fires when a `def` appears inside a `class Name:` body instead of inside a
sibling `impl Name:` block. This is Rule 4: the class body declares *data*
(fields), and `impl` blocks declare *behaviour* (methods).

## Example

```ty
class Point:
    x: int
    y: int

    def length(self) -> float:  # error: method in class body
        return (self.x * self.x + self.y * self.y) ** 0.5
```

## Why

Separating fields from methods makes generated dataclass / pydantic models
mechanical to read: every name in the class body becomes a field (and a slot),
every name in an `impl` block becomes a method.

Multiple `impl Name:` blocks at the same scope are merged by the desugarer,
so you can split methods across several blocks if it helps grouping.

## Fix

Move the `def` into a matching `impl` block:

```ty
class Point:
    x: int
    y: int

impl Point:
    def length(self) -> float:
        return (self.x * self.x + self.y * self.y) ** 0.5
```

See https://typhon.dev/lang/diagnostics/method_in_class_body
