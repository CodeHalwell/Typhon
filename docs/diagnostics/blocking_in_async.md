# tyc::blocking_in_async

Fires when a direct call to a known-blocking stdlib function
(`time.sleep`, `requests.get`, `socket.recv`, `subprocess.run`,
`input`, …) appears inside an `async def` body. The call halts the
entire event loop until it returns, defeating the point of `async`
and starving every other coroutine in the same loop.

## Example

```ty
import time
import requests

async def fetch_with_delay(url: str) -> str:
    time.sleep(1)                  # warning: blocks the event loop
    let r = requests.get(url)      # warning: blocks the event loop
    return r.text
```

## Why

`async def` only buys concurrency when the function actually yields
back to the event loop — every `await` is a yield point, every bare
synchronous call is not. Inserting a 1-second `time.sleep` into an
`async def` doesn't sleep "asynchronously"; it freezes every other
coroutine for one full second. The diagnostic catches the slip at
build time so reviewers don't have to.

The check is **direct-call only** by design: a function that itself
calls `time.sleep` internally won't be flagged transitively (that
needs the full effect-inference pipeline Phase E will land later).
Catching the common case at the syntactic call site already removes
most of the real-world misuse.

## Fix

Use the async-aware equivalent when one exists:

```ty
import asyncio
import httpx

async def fetch_with_delay(url: str) -> str:
    await asyncio.sleep(1)                       # event loop continues
    async with httpx.AsyncClient() as client:
        let r = await client.get(url)
        return r.text
```

When no async equivalent exists, wrap the blocking call so it runs
on a worker thread without occupying the event loop:

```ty
import asyncio
import time

async def reluctant_sleep() -> None:
    await asyncio.to_thread(time.sleep, 1)
```

`asyncio.to_thread(fn, ...)` is itself an async-aware wrapper and
does not trip the diagnostic — only the inner `time.sleep` would,
if it appeared directly in the async body.

The diagnostic is suppressed inside `unsafe:` regions for the rare
cases where the blocking call really is the intended behaviour
(e.g. forcing a synchronous fence inside an async test harness).

## Coverage

Current registry of blocking callees:

- **time** — `time.sleep`
- **I/O** — `input`
- **requests** — `get`, `post`, `put`, `delete`, `patch`, `head`,
  `options`, `request`
- **urllib** — `urllib.request.urlopen`
- **subprocess** — `run`, `call`, `check_call`, `check_output`

### Not yet covered

Instance-method calls like `sock.recv(1024)`, `conn.execute(...)`,
or `cursor.fetchone()` are **not** flagged, because the matcher
only sees the syntactic callee (`sock.recv`, not
`socket.socket.recv`) and the registry is keyed on the dotted
module path. Adding receiver-aware blocking detection is a
Phase-E follow-up.

## Configuration

Severity is controlled by `[strictness] blocking-in-async` in
`typhon.toml`:

```toml
[strictness]
blocking-in-async = "warn"    # default — visible but doesn't break CI
# blocking-in-async = "error" # promote for codebases on httpx/aiofiles
# blocking-in-async = "off"   # drop entirely
```

See https://typhon.dev/lang/diagnostics/blocking_in_async
