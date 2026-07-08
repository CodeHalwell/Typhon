# tyc::missing_return

Fires when a function declares a non-`None` return type but at least one
execution path reaches the end of the body without `return` or `raise`.

## Example

```ty
def classify(n: int) -> str:
    if n > 0:
        return "positive"
    if n < 0:
        return "negative"
    # falls off the end when n == 0
```

## Why

A function annotated `-> str` promises a `str` on every path. Allowing
fall-off would have Python's interpreter return `None` implicitly and the
caller would receive a `None` where it expected a `str`.

## Fix

Cover the missing path explicitly, or widen the return type to acknowledge
the `None` case.

```ty
def classify(n: int) -> str:
    if n > 0:
        return "positive"
    if n < 0:
        return "negative"
    return "zero"
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/missing_return.md
