# tyc::async_without_await

Warns when an `async def` body never uses `await`. The function still returns
a coroutine, but the `async` keyword has become a no-op — usually the sign of
a half-finished refactor or a missing `await` on an internal call.

## Example

```ty
async def fetch_count() -> int:  # warning: no `await` inside body
    return 42
```

## Why

Marking a function `async` forces every caller to either `await` it or wrap
it in `asyncio.run(...)`. When the body has no `await`, that ceremony buys
nothing while making the function awkward to call. More often it's a bug:
the author meant to `await` something inside and forgot.

## Fix

Drop `async` if the function is genuinely synchronous, or add the missing
`await` on the call that should run concurrently:

```ty
def fetch_count() -> int:
    return 42
```

See https://typhon.dev/lang/diagnostics/async_without_await
