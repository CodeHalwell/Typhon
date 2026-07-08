# tyc::typevar_bound

Fires when a generic call site infers a type argument that doesn't satisfy
its TypeVar's declared bound. Bounds are upper limits: `[T: Comparable]`
means "any subtype of `Comparable`", and a concrete `T` that doesn't satisfy
the bound is rejected at the call site.

## Example

```ty
class Comparable:
    def compare(self, other: Comparable) -> int: ...

def min[T: Comparable](a: T, b: T) -> T:
    return a if a.compare(b) <= 0 else b

def main() -> None:
    min(1, 2)  # error: `int` does not satisfy bound `Comparable`
```

## Why

Without the bound check, the body of `min` would call `.compare` on a value
that doesn't have it, and Python would raise `AttributeError` at runtime.
The bound makes that contract part of the type system: only types whose
shape is known to include the required behaviour can be passed in.

## Fix

Pass a value whose type already satisfies the bound, or remove the bound if
the body doesn't actually rely on it:

```ty
def main() -> None:
    let a: Comparable = ...
    let b: Comparable = ...
    min(a, b)
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/typevar_bound.md
