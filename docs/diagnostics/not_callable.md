# tyc::not_callable

Fires when something that is not a function (or other callable) is called.
Most commonly, a value of a non-callable type has parentheses applied to it.

## Example

```ty
def main() -> None:
    let n: int = 42
    n()  # error: `int` is not callable
```

## Why

Calling a non-callable would raise `TypeError: 'int' object is not callable`
at runtime. Catching the call statically points at the offending expression
and avoids the runtime crash.

## Fix

Remove the parentheses if you meant to read the value, or call the right
name if you meant to invoke a function:

```ty
def main() -> None:
    let n: int = 42
    print(n)
```

See https://typhon.dev/lang/diagnostics/not_callable
