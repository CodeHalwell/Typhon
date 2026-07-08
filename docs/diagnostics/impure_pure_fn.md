# tyc::impure_pure_fn

Fires when a function decorated `@pure` violates one of the six purity
conditions Typhon checks at build time.

## Example

```ty
import time

@pure
def now_plus(n: int) -> float:
    return time.time() + n  # error: reads the clock
```

## Why

`@pure` is a promise the compiler can act on: pure functions may be cached
(`@memo`, `auto-memoise`), constant-folded at comptime, or hoisted out of
loops. The six rules are:

1. The function is synchronous (no `async def`, no `await`).
2. Every argument has a hashable type.
3. No I/O — no file system, no network, no `print`.
4. No entropy or clock reads (`random`, `time.time`, `datetime.now`, ...).
5. No reads or writes of mutable module state.
6. The function does not `raise` — failure is modelled via `Result[T, E]`.

If any of these is false, the optimisations would silently change behaviour,
so the decorator is rejected.

## Fix

Either drop the `@pure` decorator if the function genuinely has side effects,
or restructure so the impure parts move to the caller:

```ty
import time

def now_plus(n: int) -> float:
    return time.time() + n

@pure
def add(a: float, b: float) -> float:
    return a + b
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/impure_pure_fn.md
