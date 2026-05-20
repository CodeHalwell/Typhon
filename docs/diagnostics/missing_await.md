# tyc::missing_await

Fires when a sync function calls an `async def` without awaiting it. The
expression's value is a coroutine object, not the function's declared
return type, and Python's runtime emits "coroutine was never awaited"
warnings for these.

## Example

```ty
async def fetch() -> int:
    return 42

def main() -> int:
    return fetch()  # error: missing `await` on async call to `fetch`
```

## Why

Calling an `async def` produces a coroutine; the body only runs when the
coroutine is awaited (or scheduled by an event loop). Treating the
coroutine as if it were the declared return value silently propagates a
wrong-typed value across the program.

## Fix

Wrap the call in `await` and make the caller `async`, or call
`asyncio.run(...)` at the top level:

```ty
import asyncio

async def main_async() -> int:
    return await fetch()

def main() -> int:
    return asyncio.run(main_async())
```

See https://typhon.dev/lang/diagnostics/missing_await
