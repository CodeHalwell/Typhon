# 9. Async and concurrency

Typhon's async story is **explicit, not inferred**. A function is async because you said so, not because the compiler decided. On top of that, two ergonomic features — `gather:` and `go` — make concurrent code shorter without sacrificing safety.

## Why explicit?

Inferring `async` (the way some languages do) means a function's "colour" can change when a deep callee changes — refactoring becomes fragile, and stack traces become confusing. Typhon stays with the explicit-`async` rule from Python, and *adds* two compile-time checks:

- An `async` function that contains no `await` is a **warning** — probably a mistake.
- A sync function that calls an `async` one without `await` is a **hard error** — definitely a mistake.

```python
async def fetch(url: str) -> str:
    ...

def main() -> None:
    val body: str = fetch("https://example.com")    # ❌ coroutine, not str
```

```
error[tyc::missing_await]: cannot use a coroutine where `str` is required
 ┌─ src/main.ty:5:21
 │
5 │     val body: str = fetch("https://example.com")
 │                     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ did you mean `await`?
                       and is `main` declared `async`?
```

## Basic async

```python
import asyncio

async def fetch(url: str) -> str:
    # imagine some I/O here
    await asyncio.sleep(0.1)
    return f"body of {url}"

async def main() -> None:
    val body: str = await fetch("https://example.com")
    print(body)

if __name__ == "__main__":
    asyncio.run(main())
```

Everything else from the language — `val`/`var`, `Result`, narrowing — works inside `async def` unchanged.

## `gather:` — run independent awaits in parallel

A common shape: several independent network calls, each behind an `await`. Sequentially awaiting them is slow when they don't depend on each other.

```python
# Sequential — 300ms if each call takes 100ms
async def load_dashboard(user_id: int) -> Dashboard:
    val user: User = await fetch_user(user_id)
    val posts: list[Post] = await fetch_posts(user_id)
    val notifs: list[Notif] = await fetch_notifs(user_id)
    return Dashboard(user=user, posts=posts, notifs=notifs)
```

`gather:` rewrites that as a parallel block:

```python
# Parallel — ~100ms
async def load_dashboard(user_id: int) -> Dashboard:
    gather:
        user   = fetch_user(user_id)
        posts  = fetch_posts(user_id)
        notifs = fetch_notifs(user_id)
    return Dashboard(user=user, posts=posts, notifs=notifs)
```

Each binding inside `gather:` is awaited in parallel. After the block, the names are in scope as their resolved values.

### How `gather:` desugars

By default, `gather:` lowers to `asyncio.TaskGroup`:

```python
# Emitted Python
async with asyncio.TaskGroup() as _tg:
    _t_user   = _tg.create_task(fetch_user(user_id))
    _t_posts  = _tg.create_task(fetch_posts(user_id))
    _t_notifs = _tg.create_task(fetch_notifs(user_id))
user   = _t_user.result()
posts  = _t_posts.result()
notifs = _t_notifs.result()
```

`TaskGroup` is the **right default**: if any task fails, the siblings are cancelled and the exception propagates. That's what you want for side-effectful work — you don't want one failed payment leaving its siblings to continue.

### Opting into best-effort semantics

If you genuinely want partial success (e.g. fanning out reads where one missing piece is fine), use `gather(strategy="best-effort"):`, which lowers to `asyncio.gather(..., return_exceptions=True)`:

```python
gather(strategy="best-effort"):
    user   = fetch_user(user_id)
    posts  = fetch_posts(user_id)
    notifs = fetch_notifs(user_id)
```

In this mode, each bound name is `T | Exception` (or similar) — the failed ones come back as exception objects you can inspect. The checker reflects that in the types.

### Automatic `asyncio.gather` (opt-in)

The analyser can rewrite *sequential* awaits as `gather` automatically, but only when:

1. Every called function is `@pure` (see [guide 10](10-advanced-features.md)).
2. LHS bindings don't alias.
3. The statements form a straight-line block.

This is **opt-in** via project config — it isn't applied silently. Most teams pick explicit `gather:` blocks instead, because they make intent visible in the source.

## `go` — fire-and-forget tasks

Sometimes you want to spawn work *without* awaiting it: a background email, a metrics flush, a write-behind cache. `go` is that primitive:

```python
async def signup(email: str) -> User:
    val user: User = await create_user(email)
    go send_welcome_email(user)         # fire and forget
    return user
```

`go` lowers through `typhon_runtime.tasks.spawn`, which keeps a strong reference to the task in a registry. This matters: Python's event loop holds only **weak** references to tasks, so a bare `asyncio.create_task(...)` whose handle is dropped can be garbage-collected mid-flight. The Typhon runtime helper prevents that.

### Capturing the handle

If you want to join later, bind the task:

```python
async def signup(email: str) -> User:
    val user: User = await create_user(email)
    go send_welcome_email(user) -> email_task
    # ... later ...
    await email_task
    return user
```

The type of `email_task` matches the underlying spawn primitive (an `asyncio.Task` in async contexts, a `Future` on free-threaded builds for CPU-bound work).

## Free-threaded Python (3.13t / 3.14t)

Set `free-threaded = true` in `typhon.toml` to opt into emit paths that use threads (not just async tasks) for parallelism:

```toml
[python]
target = "3.14"
free-threaded = true
```

When this is on:

- `go` on a CPU-bound function lowers to a `ThreadPoolExecutor.submit` future instead of an `asyncio.Task`.
- The analyser may emit `ThreadPoolExecutor.map(...)` for pure-function comprehensions over large collections.
- Every emitted parallel block first checks `sys._is_gil_enabled()` at runtime and falls back to sequential execution if a GIL build is detected.

This is **default-off** until 3.14 ships as the default Python.

### Why the runtime fallback?

Free-threaded mode requires a special CPython build. If your wheel is run on a stock GIL Python, the threading parallelism would serialize anyway — worse, it might trigger race-related bugs the GIL had been hiding. The fallback keeps the code correct on any 3.13+/3.14+ interpreter.

## `await` inside loops

A common shape that *isn't* a fit for `gather`:

```python
async def process(urls: list[str]) -> list[str]:
    var results: list[str] = []
    for url in urls:
        val body: str = await fetch(url)
        results.append(body)
    return results
```

The `await` is sequential and order-dependent — `gather:` won't help here. If you want concurrent processing of a list, do it explicitly with `asyncio.gather(*[...])` (still available) or a `TaskGroup` over an iterable.

## Putting it together

A small example: fetch user data from three independent endpoints in parallel, spawn a fire-and-forget metrics call, and surface a typed error if any fetch fails.

```python
import asyncio

class User:
    id: int
    name: str
class Post:
    id: int
    title: str
class Notif:
    id: int
    text: str

type LoadError = NotFound | Timeout
class NotFound:
    what: str
class Timeout:
    after_ms: int

async def fetch_user(id: int) -> Result[User, LoadError]: ...
async def fetch_posts(id: int) -> Result[list[Post], LoadError]: ...
async def fetch_notifs(id: int) -> Result[list[Notif], LoadError]: ...
async def record_load(id: int, ms: int) -> None: ...

class Dashboard:
    user: User
    posts: list[Post]
    notifs: list[Notif]

async def load_dashboard(user_id: int) -> Result[Dashboard, LoadError]:
    val started: float = asyncio.get_event_loop().time()

    gather:
        user_r   = fetch_user(user_id)
        posts_r  = fetch_posts(user_id)
        notifs_r = fetch_notifs(user_id)

    val user: User = user_r?
    val posts: list[Post] = posts_r?
    val notifs: list[Notif] = notifs_r?

    val elapsed_ms: int = int((asyncio.get_event_loop().time() - started) * 1000)
    go record_load(user_id, elapsed_ms)

    return Ok(Dashboard(user=user, posts=posts, notifs=notifs))
```

Walk through:

- The three `fetch_*` calls run in parallel inside `gather:` — total latency is `max`, not `sum`.
- Each binding is still `Result[...]`. The `?` operator unwraps them after the gather; if any returned `Err`, this function returns that error.
- `record_load` is fire-and-forget via `go`. The metrics call doesn't block the response.
- The whole thing is `async` and returns a `Result`, so error handling and async compose without ceremony.

## Common mistakes

**Forgetting `await`:**

```python
async def main() -> None:
    val body: str = fetch("...")    # ❌ missing await
```

`tyc check` flags this. Add `await`.

**`async` function with no `await`:**

```python
async def helper() -> int:
    return 42
```

```
warning[tyc::async_without_await]: `helper` is `async` but never awaits;
                                   make it sync, or `await` something inside it
```

Either drop `async` or actually await something. Don't suppress this — it's almost always wrong.

**Using `asyncio.create_task` directly for fire-and-forget:**

```python
asyncio.create_task(send_welcome_email(user))    # ⚠️ task may be GC'd before completion
```

Fix: `go send_welcome_email(user)` — the runtime registry holds a strong reference.

**Putting blocking I/O inside an `async` function:**

```python
async def load() -> bytes:
    with open("data.bin", "rb") as f:
        return f.read()    # blocks the event loop
```

Typhon doesn't catch this (it's a Python-wide hazard). Use `aiofiles` or `asyncio.to_thread(...)` for blocking calls in async code.

## What you've learned

- `async` is explicit; missing `await` is a hard error, redundant `async` is a warning.
- `gather:` blocks run independent awaits in parallel, defaulting to `TaskGroup` (cancel-on-failure).
- `gather(strategy="best-effort"):` switches to `asyncio.gather(..., return_exceptions=True)`.
- `go f(x)` spawns fire-and-forget work safely via a strong-ref registry.
- Free-threaded mode opens threading-based parallelism on 3.13t/3.14t with a runtime fallback to sequential.

Next: [Advanced features](10-advanced-features.md) — pipes, `comptime`, lazy imports, purity, and the `unsafe` boundary.
