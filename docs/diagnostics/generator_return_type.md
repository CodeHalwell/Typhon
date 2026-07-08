# tyc::generator_return_type

Fires when a function body contains `yield` (or `yield from`) but its declared
return type isn't iterator-shaped. Calling such a function returns a generator
object at runtime, not the declared type.

## Example

```ty
def counts() -> list[int]:  # error: this returns a generator, not list[int]
    yield 1
    yield 2
```

## Why

Python's `def` quietly switches semantics the moment a body uses `yield`: the
function no longer returns the expression on its `return` line — it returns a
generator that yields values. Mismatching the annotation therefore silently
type-launders an `int` as a `list[int]` and breaks every caller.

## Fix

Annotate the return type with the correct iterator type. Use `Iterator[T]` or
`Generator[T, S, R]` for sync, and `AsyncIterator[T]` / `AsyncGenerator[T, S]`
for `async def`:

```ty
from collections.abc import Iterator

def counts() -> Iterator[int]:
    yield 1
    yield 2
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/generator_return_type.md
