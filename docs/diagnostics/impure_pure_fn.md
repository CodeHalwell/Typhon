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

The check is syntactic and reports only what it can *prove*: I/O and clock /
entropy builtins under any import alias (`datetime.now()` after
`from datetime import datetime`, `np.random.rand()`), logging calls, I/O
method names on any receiver (`.read_text()`, `.write()`, `.send()`),
mutation of an argument or a module binding through a method or an attribute
write (`REGISTRY.append(x)`, `c.n = c.n + 1`, `next(it)` on a parameter), a
read of a `mut` (or rebound) module binding, and calls to same-module helpers
that are not `@pure`. A call the checker cannot classify — a method on a value
of unknown type, a function from a module outside the pure stdlib allow-list
— is trusted under `@pure`; the automatic optimisations (`auto-memoise`,
`pgo-memoise`, `auto-parallel`) require provable purity and ignore such
functions. See [language.md — What the verifier
proves](../language.md#what-the-verifier-proves).

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
