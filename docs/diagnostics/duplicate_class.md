# tyc::duplicate_class

Fires when the same class name is declared more than once at the same scope.
Python silently lets the second definition shadow the first; Typhon rejects
the duplicate so the user can either rename one or merge the bodies.

## Example

```ty
class Point:
    x: int

class Point:  # error: class `Point` is declared more than once
    y: int
```

## Why

A silent redeclaration would drop every method or field on the first body
without warning, leading to baffling "missing attribute" errors elsewhere.
Catching it at the declaration site makes the conflict obvious.

## Fix

Rename one of the declarations, or merge the second body into the first.
Behavioural extensions belong in a sibling `impl Point:` block; additional
fields belong directly inside the original class body:

```ty
class Point:
    x: int
    y: int

impl Point:
    def distance(self) -> float:
        return (self.x * self.x + self.y * self.y) ** 0.5
```

See https://typhon.dev/lang/diagnostics/duplicate_class
