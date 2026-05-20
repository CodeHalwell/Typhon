# tyc::type_mismatch

Fires when an expression of one type is supplied where a different type was
expected — function arguments, return values, assignments, container element
types, etc.

## Example

```ty
def double(n: int) -> int:
    return n * 2

def main() -> None:
    let result: int = double("3")  # error: expected `int`, found `str`
    print(result)
```

A reassignment variant fires when `mut x: T = ...` is followed by
`x = <value of some other type>`. `mut` allows new values of the same
declared type, never a re-typing.

## Why

Typhon's type checker is strict-by-default: an annotation is a contract, not
a hint. Permitting an `str` where `int` was promised would silently propagate
a wrong-typed value across the program and surface as a runtime `TypeError`
far from the source of the problem.

## Fix

Convert the expression to the expected type, or update the surrounding
annotation if the call site is the source of truth:

```ty
def main() -> None:
    let result: int = double(int("3"))  # ok
    print(result)
```

See https://typhon.dev/lang/diagnostics/type_mismatch
