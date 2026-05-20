# tyc::arg_count

Fires when a function is called with the wrong number of positional arguments
for its declared signature.

## Example

```ty
def add(a: int, b: int) -> int:
    return a + b

def main() -> None:
    add(1, 2, 3)  # error: expected 2, got 3
```

## Why

Typhon's checker computes the expected arity from the function's parameters
(including `*args`/`**kwargs` if present) and rejects call sites whose
positional count can't be matched. Catching the count mismatch at check time
avoids `TypeError` at runtime.

## Fix

Pass the correct number of positional arguments, or move extras into keyword
arguments / a `*args` parameter:

```ty
def main() -> None:
    add(1, 2)
```

See https://typhon.dev/lang/diagnostics/arg_count
