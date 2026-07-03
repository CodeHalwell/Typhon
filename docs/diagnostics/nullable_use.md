# tyc::nullable_use

Fires when a value of type `T | None` (a "nullable") is used where the
surrounding context demands `T`. Typhon writes the nullable shorthand `T?`
which means exactly the same thing as `T | None`.

## Example

```ty
def length_of(name: str?) -> int:
    return len(name)  # error: possibly-None value used where `str` is required
```

## Why

`None` is its own type. Calling `len(None)` or passing `None` to a function
that expects `str` raises at runtime, so the type checker forbids the use
until the value has been narrowed.

## Fix

Guard the value with an `is not None` check; the checker narrows the binding
inside the branch.

```ty
def length_of(name: str?) -> int:
    if name is not None:
        return len(name)
    return 0
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/nullable_use.md
