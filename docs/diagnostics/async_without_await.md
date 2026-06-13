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

## When it does not fire

The warning is suppressed when the `async def` has a legitimate reason to be
`async` despite awaiting nothing:

- **Async-protocol dunders** — `__aenter__` / `__aexit__` / `__aiter__` /
  `__anext__` (e.g. an `__aenter__` that just returns `self`).
- **Declaration-only bodies** — `...` / `pass` / a bare docstring, as written
  in `interface` / Protocol method signatures.
- **Async generators** — a body containing `yield` is a coroutine by nature.
- **Contract-required async methods** — a method that is `async` only because
  it implements an `interface` whose same-named method is `async def` (and the
  class structurally conforms), or because it overrides an `async def` method
  of the same name on a base class. Such a method *cannot* drop `async`
  without breaking the contract, so warning on it would only force a dead
  `await asyncio.sleep(0)` no-op into an otherwise-correct trivial impl:

  ```ty
  interface Sink:
      async def deliver(self, msg: str) -> None

  class ConsoleSink:
      prefix: str

  impl ConsoleSink:
      async def deliver(self, msg: str) -> None:  # no warning — required async
          print(self.prefix + msg)
  ```

  This is gated on the *interface* method being `async`: an `async` impl of a
  *sync* interface method is async by choice, so it still warns.

See https://typhon.dev/lang/diagnostics/async_without_await
