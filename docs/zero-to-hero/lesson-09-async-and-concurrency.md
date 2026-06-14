# Lesson 9 — Async and concurrency

*Zero to Hero · Lesson 9 of 10*

`async`/`await` work as in Python, but Typhon makes concurrency *explicit* and adds clean syntax for the two things you actually do: run independent work in parallel, and fire-and-forget.

## Explicit `async`

- Calling an `async def` from a sync context without `await` is a **hard error** (`tyc::missing_await`).
- An `async def` with no `await` inside is a **warning** (`tyc::async_without_await`).
- A known-blocking call (`time.sleep`, `requests.get`, `subprocess.run`, …) inside `async def` is flagged (`tyc::blocking_in_async`) — wrap it in `asyncio.to_thread(...)` or use an async-native client.

```python
import asyncio

async def fetch_user(uid: int) -> str:
    await asyncio.sleep(0.1)
    return f"user-{uid}"
```

## `gather:` — concurrent awaits

When several awaits are independent, run them concurrently with a `gather:` block:

```python
async def load(uid: int) -> Dashboard:
    gather:
        user   = fetch_user(uid)
        posts  = fetch_posts(uid)
        notifs = fetch_notifs(uid)
    return Dashboard(user=user, posts=posts, notifs=notifs)
```

This lowers to an `asyncio.TaskGroup` (cancel-on-failure). The bindings inside `gather:` don't need `let`/`mut` — the keyword introduces them as single-assignment names. If one binding references another, the block gracefully degrades to sequential awaits in source order.

For "run them all, give me successes *and* failures," use best-effort mode (lowers to `asyncio.gather(..., return_exceptions=True)`, making each binding `T | Exception`):

```python
gather(strategy="best-effort"):
    user = fetch_user(uid)
    posts = fetch_posts(uid)
```

## `go` — fire-and-forget

To launch background work you don't await, use `go` — **never** a bare `asyncio.create_task`. Python's event loop holds only weak references to tasks, so a fire-and-forget task can be garbage-collected mid-flight; `go` routes through a runtime registry that keeps a strong reference until the task completes:

```python
async def signup(email: str) -> User:
    let user: User = await create(email)
    go send_welcome(user)        # fire-and-forget, kept alive by the runtime
    return user
```

Capture the handle if you want to await it later:

```python
go send_welcome(user) -> task
await task
```

## Common mistakes

**Calling an async function without `await`:**

```python
def main() -> None:
    let u = fetch_user(1)        # ❌ tyc::missing_await
```

Fix: make the caller `async` and `await` the call (and run it with `asyncio.run(...)`).

**Blocking inside async:**

```python
async def slow() -> None:
    time.sleep(1)                # ⚠ tyc::blocking_in_async
    await asyncio.sleep(1)       # ✅ async-native
```

**Bare `create_task` for fire-and-forget** — works until the task vanishes under GC. Use `go`.

## Try it

1. Write two `async def` functions that each `await asyncio.sleep(...)` and return a number. Sum their results inside a `gather:` block.
2. Add a third call that depends on the first's result and confirm Typhon sequences it correctly.
3. Add a `go log_event(...)` fire-and-forget call and run with `tyc run`.

## What you learned

- Concurrency is explicit: missing `await` and blocking-in-async are diagnostics, not silent bugs.
- `gather:` runs independent awaits concurrently via `TaskGroup`; `strategy="best-effort"` collects failures.
- `go` is the safe fire-and-forget spawn — never bare `asyncio.create_task`.

**Next:** [Lesson 10 — Boundaries, power tools, and a capstone](lesson-10-boundaries-and-capstone.md).
