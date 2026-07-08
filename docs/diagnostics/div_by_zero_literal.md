# tyc::div_by_zero_literal

Fires when a division-style operator (`/`, `//`, `%`) has a literal
zero on the right-hand side. The runtime raises `ZeroDivisionError`
unconditionally, so catching the case at compile time is a free win.

## Example

```ty
def average(values: list[float]) -> float:
    # error: division by literal zero — `x / 0` always raises ZeroDivisionError
    return sum(values) / 0
```

## Why

A literal zero divisor has no defensible interpretation — the
expression cannot reach production without immediately raising. The
check is **constant-fold only**: it fires for `0`, `0.0`, `-0`,
`-0.0`, and the unary-negation forms, but never for runtime values
that might be zero. Flow-sensitive analysis (e.g. recognising that
`if d != 0: x / d` is safe) is out of scope by design — the zero
false-positive guarantee depends on staying narrow.

The check skips bodies inside `unsafe:` regions, like every other
diagnostic.

## Fix

Change the divisor to a non-zero literal, or guard the expression
behind a runtime check:

```ty
def average(values: list[float]) -> float?:
    if len(values) == 0:
        return None
    return sum(values) / len(values)
```

If the literal zero is intentional (testing the error path, for
example), wrap the expression in an `unsafe:` block:

```ty
def trigger_error() -> None:
    unsafe:
        _ = 1 / 0
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/div_by_zero_literal.md
