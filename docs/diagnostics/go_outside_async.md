# tyc::go_outside_async

**Error.** A `go f(x)` spawn runs from module-level code — directly, or
through a module-level call to a synchronous function that spawns — where
no event loop is running.

## Example

```ty
import asyncio

async def work() -> None:
    print("w")

def kick() -> None:
    go work()        # error: no event loop is running here

kick()
```

## Why

`go f(x)` lowers to `typhon_runtime.tasks.spawn(f(x))`, which schedules the
coroutine on the *running* event loop. A synchronous function — or
module-level code — has no running loop, so the spawn raised
`RuntimeError: no running event loop` at the call, and the coroutine it had
already created was never awaited (`RuntimeWarning: coroutine 'work' was
never awaited`). The program built cleanly and only failed at runtime; the
diagnostic moves that failure to `tyc check`.

The rule is deliberately narrow, because a synchronous function *can* spawn
when it is called from a coroutine (the loop is running then): only a `go`
in module-level code, and a module-level call to a top-level sync function
whose own body contains a `go`, are flagged. In the example the error sits
on the `kick()` call, naming `work` as the coroutine that would have been
spawned.

## Fix

Either move the spawn into an `async def` and drive that with
`asyncio.run(...)`:

```ty
import asyncio

async def work() -> None:
    print("w")

async def kick() -> None:
    go work()
    await asyncio.sleep(0)

asyncio.run(kick())
```

or, when the calling code is synchronous by design, run the coroutine to
completion instead of spawning it:

```ty
def kick() -> None:
    asyncio.run(work())
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/go_outside_async.md
