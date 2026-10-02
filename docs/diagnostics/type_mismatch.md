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


## `await` on a value that is not awaitable

`await` needs a coroutine, a task, a future, or an object with `__await__`.
Awaiting the result of a *sync* function or method declared in the same
module, or a literal, is `TypeError: object int can't be used in 'await'
expression` at runtime and is reported here since the 2026-09-30 review. A
bare name, a call through a `Callable`, or a function imported from another
module is left alone — the checker types an un-awaited coroutine by its
result, so it cannot tell those apart:

```ty
def load() -> int:
    return 1

async def main() -> None:
    let v: int = await load()   # error: expected `an awaitable`, found `int`
```

## Subclass field redeclarations

A subclass may redeclare an inherited field (to add a default, say), but not
with an incompatible type: code written against the base then reads a value
of the wrong type.

```ty
class Base:
    x: int

class Sub(Base):
    x: str          # error: expected `int`, found `str`
    y: int = 0      # fine — a new field
```

## `for` over a non-iterable

Iterating an instance of a class that defines no `__iter__` (and whose
hierarchy is fully known) is `TypeError: 'Bag' object is not iterable`, and is
reported here.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/type_mismatch.md
