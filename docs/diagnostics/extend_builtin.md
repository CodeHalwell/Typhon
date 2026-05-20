# tyc::extend_builtin

Fires when an `extend` declaration names a Python built-in type (`int`, `str`,
`list`, etc.). Built-ins are implemented in C and cannot be modified at
runtime, so the `extend` block would silently have no effect.

## Example

```ty
extend int:  # error: cannot extend a built-in type
    def doubled(self) -> int:
        return self * 2
```

## Why

CPython's built-in types are immutable from Python land — `int.doubled = ...`
raises `TypeError: cannot set attribute`. Allowing an `extend` block on a
built-in would generate code that crashes at import, so the check fires at
declaration time instead.

## Fix

Wrap the built-in in a user-defined class, or expose the extension as a free
function that takes the built-in value as a parameter:

```ty
def doubled(n: int) -> int:
    return n * 2
```

See https://typhon.dev/lang/diagnostics/extend_builtin
